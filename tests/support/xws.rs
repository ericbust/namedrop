#![allow(dead_code)]

use crate::cleanup;

use namedrop::{
    ActorKind, AgentRuntime, AgentSessionId, Clock, IdGenerator, LifecycleRuntime,
    MutationAttribution, PaneAddressDraft, PaneAddressId, PaneBindingId, PaneRoleId,
    StateNamespace, StateStore, complete_capability_profile, drain_mailboxes,
    execute_control_operation, execute_integration_operation, execute_lifecycle_operation,
    execute_message_operation, execute_user_prompt_submit_hook, issue_registration_challenge,
    mux::{
        AgentState, Depth, Effect, InputOutcome, MuxError, PaneProcess,
        fake::{
            Call, FakeMux, Listing, Read, ScopeKind, SentInput, StrictSpace, Surveyed, Takes, runs,
        },
    },
    spaces_to_wake,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
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

/// The switches and records of the old executor. Tests flip and read them
/// through `FakeHerdr`, and the space's hooks apply and fill them.
#[derive(Clone, Default)]
pub struct Herdr {
    pub repo: String,
    pub labels: Arc<Mutex<Vec<(String, String)>>>,
    /// The repository Herdr stamps on a workspace's checkout, by workspace id.
    pub repos: Arc<Mutex<Vec<(String, String)>>>,
    pub workspace_list_fails: Arc<AtomicBool>,
    pub prompts: Arc<Mutex<Vec<(String, String)>>>,
    /// Each notification's title and body.
    pub notifications: Arc<Mutex<Vec<Vec<String>>>>,
    /// Every read, input, and effect the code tried, as the old executor
    /// logged every command, so a test can clear it.
    pub commands: Arc<Mutex<Vec<Call>>>,
    pub statuses: Arc<Mutex<BTreeMap<String, String>>>,
    pub sessions: Arc<Mutex<BTreeMap<String, String>>>,
    pub unreadable: Arc<Mutex<BTreeSet<String>>>,
    pub missing_panes: Arc<Mutex<BTreeSet<String>>>,
    pub failing_prompts: Arc<Mutex<BTreeSet<String>>>,
    /// Shell panes as (workspace, pane, physical terminal).
    pub terminals: Arc<Mutex<Vec<(String, String, String)>>>,
    pub runs: Arc<Mutex<Vec<(String, String)>>>,
    pub shells_busy: Arc<AtomicBool>,
    /// Every shell pane a switch ever opened in the world.
    opened: Arc<Mutex<BTreeSet<String>>>,
}

/// The three workspaces of `SPACES`. The old executor answered `workspace
/// list` (each space's label and number, and a stamp for each space in
/// `repos`), `worktree list --workspace` (the repository for w1 and
/// `not_git_worktree` for the others), `pane list --workspace`, and `pane
/// process-info` for shell panes. It took `agent prompt` and `pane run` to
/// any pane, `notification show`, and `pane report-metadata`, and it failed
/// every other command, `worktree list --cwd`, `pane read`, and `agent
/// send-keys` among them. A read hook applies the switches to the world
/// before every read, and `reply` applies them before it renders one.
pub struct FakeHerdr {
    herdr: Herdr,
    pub space: StrictSpace,
}

impl std::ops::Deref for FakeHerdr {
    type Target = Herdr;

    fn deref(&self) -> &Herdr {
        &self.herdr
    }
}

/// Whether `call` lists the workspaces, Herdr's `workspace list`. The space
/// answers no checkout survey, so only a survey of every space at the
/// spaces depth runs it.
pub fn lists_spaces(call: &Call) -> bool {
    matches!(
        call,
        Call::Read(Read::Survey {
            scope: Surveyed::All,
            depth: Depth::Spaces
        })
    )
}

/// Whether `call` ran a command line in a pane, Herdr's `pane run`.
pub fn runs_line(call: &Call) -> bool {
    matches!(call, Call::Input(attempt) if matches!(attempt.input, SentInput::Line(_)))
}

impl FakeHerdr {
    pub fn new(repo: &std::path::Path) -> Self {
        let herdr = Herdr {
            repo: repo.display().to_string(),
            labels: Arc::new(Mutex::new(
                SPACES
                    .iter()
                    .map(|(id, label, _)| ((*id).to_owned(), (*label).to_owned()))
                    .collect(),
            )),
            ..Herdr::default()
        };
        let mut world = FakeMux::world().endpoint("socket-a");
        for (id, label, agents) in SPACES {
            world = world.space(id).label(label).unstamped();
            if id == "w1" {
                world = world.checkout(repo);
            }
            for (index, (_, pane)) in agents.iter().enumerate() {
                world = world
                    .pane(pane)
                    .terminal(&format!("t-{pane}"))
                    .tab(&format!("tab-{index}"))
                    .codex()
                    .session(&session(pane))
                    .idle();
            }
        }
        let mux = world.build();
        for (_, _, agents) in SPACES {
            for (_, pane) in agents {
                mux.answer(pane, InputOutcome::Delivered);
            }
        }
        let surveys = [
            (ScopeKind::All, Depth::Spaces),
            (ScopeKind::Space, Depth::Panes),
            (ScopeKind::PanesOf, Depth::Spaces),
            (ScopeKind::PanesOf, Depth::Panes),
        ];
        let takes = Takes::nothing()
            .input(|attempt| matches!(attempt.input, SentInput::Prompt(_) | SentInput::Line(_)))
            .effect(|effect| {
                matches!(
                    effect,
                    Effect::Notify { .. } | Effect::Identify { .. } | Effect::Badges { .. }
                )
            });
        let fake = Self {
            space: StrictSpace::new(mux, &surveys, takes),
            herdr,
        };
        fake.hook();
        fake
    }

    fn hook(&self) {
        let herdr = self.herdr.clone();
        self.space.on_read(move |mux, read| {
            herdr
                .commands
                .lock()
                .unwrap()
                .push(Call::Read(read.clone()));
            herdr.sync(mux);
            if lists_spaces(&Call::Read(read.clone()))
                && herdr.workspace_list_fails.load(Ordering::Relaxed)
            {
                return Err(MuxError::Unavailable(
                    "herdr: workspace list is unavailable".to_owned(),
                ));
            }
            let listed = match read {
                Read::Survey {
                    scope: Surveyed::Space(space) | Surveyed::PanesOf(space),
                    ..
                } if runs(read, Listing::PanesOf) => Some(space.as_str()),
                _ => None,
            };
            if let Some(space) = listed
                && herdr.unreadable.lock().unwrap().contains(space)
            {
                return Err(MuxError::Unavailable(format!(
                    "herdr: workspace {space} is unavailable"
                )));
            }
            Ok(())
        });
        let failing = Arc::clone(&self.herdr.failing_prompts);
        self.space.answer_with(move |_, pane, input| {
            (matches!(input, SentInput::Prompt(_))
                && failing.lock().unwrap().contains(pane.as_str()))
            .then(|| InputOutcome::Refused("definitive".to_owned()))
        });
        let herdr = self.herdr.clone();
        self.space.on_input(move |_, attempt| {
            herdr
                .commands
                .lock()
                .unwrap()
                .push(Call::Input(attempt.clone()));
            if attempt.outcome != InputOutcome::Delivered {
                return;
            }
            let pane = attempt.pane.as_str().to_owned();
            match &attempt.input {
                SentInput::Prompt(prompt) => {
                    herdr.prompts.lock().unwrap().push((pane, prompt.clone()))
                }
                SentInput::Line(line) => herdr.runs.lock().unwrap().push((pane, line.clone())),
                SentInput::Keys { .. } => {}
            }
        });
        let herdr = self.herdr.clone();
        self.space.on_apply(move |_, effect| {
            herdr
                .commands
                .lock()
                .unwrap()
                .push(Call::Effect(effect.clone()));
            if let Effect::Notify { title, body, .. } = effect {
                herdr
                    .notifications
                    .lock()
                    .unwrap()
                    .push(vec![title.clone(), body.clone()]);
            }
            Ok(())
        });
    }

    /// What Herdr prints for `args`, for a script `herdr` that serves the
    /// dashboard. It applies the switches, `workspace_list_fails` and
    /// `unreadable` included, and answers only the three listings the old
    /// executor answered, failing any other command as it did. The test
    /// reads the reply for itself, so it is not in `commands` and runs no
    /// hook.
    pub fn reply(&self, args: &[&str]) -> Result<String, String> {
        self.herdr.sync(&self.space);
        match args {
            ["workspace", "list"] if self.workspace_list_fails.load(Ordering::Relaxed) => {
                Err("herdr: workspace list is unavailable".to_owned())
            }
            ["pane", "list", "--workspace", space]
                if self.unreadable.lock().unwrap().contains(*space) =>
            {
                Err(format!("herdr: workspace {space} is unavailable"))
            }
            ["workspace", "list"]
            | ["worktree", "list", "--workspace", _, "--json"]
            | ["pane", "list", "--workspace", _] => self.space.herdr_reply(args),
            _ => Err(format!("unexpected Herdr command: {args:?}")),
        }
    }
}

impl Herdr {
    /// Applies the switches to the world.
    fn sync(&self, mux: &FakeMux) {
        let repos = self.repos.lock().unwrap().clone();
        for (space, label) in self.labels.lock().unwrap().iter() {
            mux.set_label(space, Some(label));
            let stamp = repos
                .iter()
                .find(|(stamped, _)| stamped == space)
                .map(|(_, repo)| (PathBuf::from(format!("/worktrees/{repo}/{label}")), repo));
            mux.set_stamp(
                space,
                stamp
                    .as_ref()
                    .map(|(path, repo)| (path.as_path(), Some(repo.as_str()))),
            );
        }
        let statuses = self.statuses.lock().unwrap().clone();
        let sessions = self.sessions.lock().unwrap().clone();
        let missing = self.missing_panes.lock().unwrap().clone();
        for (_, _, agents) in SPACES {
            for (_, pane) in agents {
                let status = statuses.get(*pane).map_or("idle", String::as_str);
                mux.set_state(pane, AgentState::parse(status));
                let session = sessions
                    .get(*pane)
                    .cloned()
                    .unwrap_or_else(|| session(pane));
                mux.set_session(pane, &session);
                if missing.contains(*pane) {
                    mux.hide_pane(pane);
                } else {
                    mux.show_pane(pane);
                }
            }
        }
        let group = if self.shells_busy.load(Ordering::Relaxed) {
            43
        } else {
            42
        };
        let terminals = self.terminals.lock().unwrap().clone();
        let mut opened = self.opened.lock().unwrap();
        for (space, pane, terminal) in &terminals {
            if opened.insert(pane.clone()) {
                mux.open_pane(space, |space| {
                    space
                        .pane(pane)
                        .terminal(terminal)
                        .tab("tab-shell")
                        .state(AgentState::parse("unknown"))
                        .reports_session(false)
                });
                mux.answer(pane, InputOutcome::Delivered);
            }
            mux.show_pane(pane);
            mux.set_terminal(pane, terminal);
            mux.set_process(
                pane,
                Some(PaneProcess {
                    shell_pid: Some(42),
                    foreground_group: Some(group),
                    foreground_pids: vec![std::process::id()],
                    tty: None,
                }),
            );
        }
        for pane in opened.iter() {
            if !terminals.iter().any(|(_, shell, _)| shell == pane) {
                mux.hide_pane(pane);
                mux.set_process(pane, None);
            }
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
            .map(|(_, prompt)| crate::delivered::envelope_of(prompt))
            .filter(|prompt| prompt.get("control").is_some())
            .collect()
    }

    pub fn last_envelope(&self, pane: &str) -> Value {
        self.envelopes(pane)
            .pop()
            .unwrap_or_else(|| panic!("no envelope was dispatched to {pane}"))
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
impl TestClock {
    /// Starts at the real time and moves only when a test advances it. The
    /// dashboard and the workers it starts read the real clock over the same
    /// state, and they hold as expired anything stamped a day before their
    /// own time.
    fn starting_now() -> Self {
        Self(AtomicU64::new(namedrop::SystemClock.now_unix_ms()))
    }
}
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

pub type Runtime<'a> = LifecycleRuntime<'a, FakeMux, TestClock, SequenceIds>;

impl Fixture {
    fn as_typed(&self, prompt: &str) -> String {
        let typed = self.herdr.prompts.lock().unwrap();
        crate::delivered::as_typed(typed.iter().map(|(_, typed)| typed.as_str()), prompt)
    }

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
            clock: TestClock::starting_now(),
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
            mux: &self.herdr.space,
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

    /// The mailbox worker's pass after the CLI commits a send, reply or complete.
    pub fn wake(&self, workspace: &str) {
        for space in spaces_to_wake(&self.state, "socket-a", workspace) {
            self.drain(&space);
        }
    }

    pub fn agent_send(&self, workspace: &str, pane: &str, request: &str, source: &str) -> Value {
        let sent = execute_message_operation(
            "send",
            json!({"request_id":request,"source":source,"author_kind":"agent_session"}),
            &self.runtime(workspace, pane),
        )
        .unwrap();
        self.wake(workspace);
        sent
    }

    pub fn reply_to(&self, workspace: &str, pane: &str, envelope: &Value, source: &str) -> Value {
        let replied = execute_message_operation(
            "reply",
            json!({"request_id":format!("reply-{}", envelope["control"]["attempt_id"]),
                   "source":source,"author_kind":"agent_session",
                   "in_reply_to":envelope["control"]["message_id"],
                   "attempt_id":envelope["control"]["attempt_id"]}),
            &self.runtime(workspace, pane),
        )
        .unwrap();
        self.wake(workspace);
        replied
    }

    pub fn try_send(&self, workspace: &str, pane: &str, input: Value) -> Result<Value, String> {
        execute_message_operation("send", input, &self.runtime(workspace, pane))
    }

    pub fn hook(&self, workspace: &str, pane: &str, prompt: &str) -> Option<Value> {
        let prompt = self.as_typed(prompt);
        execute_user_prompt_submit_hook(
            &json!({"hook_event_name":"UserPromptSubmit","prompt":prompt}).to_string(),
            &self.runtime(workspace, pane),
        )
    }

    /// The hook as a host calls it, with its own session id on stdin.
    pub fn hook_as(
        &self,
        session: &str,
        workspace: &str,
        pane: &str,
        prompt: &str,
    ) -> Option<Value> {
        execute_user_prompt_submit_hook(
            &json!({"hook_event_name":"UserPromptSubmit","prompt":prompt,"session_id":session})
                .to_string(),
            &self.runtime(workspace, pane),
        )
    }

    /// A delivery typed into `pane` as the prompt of Codex turn `turn`.
    pub fn delivered_in_turn(
        &self,
        workspace: &str,
        pane: &str,
        turn: &str,
        envelope: &Value,
    ) -> Option<Value> {
        let prompt = self.as_typed(&envelope.to_string());
        execute_user_prompt_submit_hook(
            &json!({"hook_event_name":"UserPromptSubmit","prompt":prompt,
                    "session_id":session(pane),"turn_id":turn})
            .to_string(),
            &self.runtime(workspace, pane),
        )
    }

    pub fn now(&self) -> u64 {
        self.clock.now_unix_ms()
    }

    pub fn advance(&self, ms: u64) {
        self.clock.0.fetch_add(ms, Ordering::Relaxed);
    }

    /// Moves the clock back, so what the fixture stamps next is already `ms`
    /// old to a dashboard reading the real clock.
    pub fn rewind(&self, ms: u64) {
        self.clock.0.fetch_sub(ms, Ordering::Relaxed);
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

pub fn stop_workers(state: &std::path::Path) {
    use std::{
        thread,
        time::{Duration, Instant},
    };
    let queue = state.join("agent-status-reconciliation");
    let mut known: BTreeSet<_> = fixture_workers(state).into_iter().collect();
    let _ = fs::remove_dir_all(&queue);
    let deadline = Instant::now() + Duration::from_secs(15);
    let stop_deadline = deadline + Duration::from_secs(5);
    loop {
        let workers = fixture_workers(state);
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

impl Drop for Fixture {
    fn drop(&mut self) {
        stop_workers(&self.state);
        let _ = cleanup::remove_fixture(&self.root);
    }
}
