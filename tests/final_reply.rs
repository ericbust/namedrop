//! A delivery settled from the agent's final message: the prompt hook arms a
//! claim and says so, and the Stop runs the bound operation the CLI runs.
#![cfg(feature = "test-support")]
#[path = "support/cleanup.rs"]
mod cleanup;
#[path = "support/delivered.rs"]
mod delivered;
#[path = "support/dispatch.rs"]
mod dispatch;
#[path = "support/executable.rs"]
mod executable;

use namedrop::{
    ActorKind, AgentRuntime, AgentSessionId, Clock, IdGenerator, LifecycleRuntime,
    MutationAttribution, PaneAddressDraft, PaneAddressId, PaneBindingId, PaneRoleId,
    StateNamespace, StateStore, complete_capability_profile, execute_integration_operation,
    execute_lifecycle_operation, execute_message_operation, execute_session_start_hook,
    execute_user_prompt_submit_hook, expand_attempt_reply,
    final_reply::{BoundOperation, FinalText, HostTurn, StopFacts, mark_stopped, settle_at_stop},
    issue_registration_challenge,
    mux::{
        AgentKind, Depth, Effect, InputOutcome, MuxError,
        fake::{
            CHECKOUT_LISTINGS, FakeMux, Read, ScopeKind, SentInput, StrictSpace, Surveyed, Takes,
        },
    },
    send_operator_message,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

struct TestDirectory {
    root: PathBuf,
    repo: PathBuf,
    state: PathBuf,
    home: PathBuf,
}

impl TestDirectory {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "namedrop-final-reply-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        let (repo, state, home) = (root.join("repo"), root.join("state"), root.join("home"));
        for path in [&repo, &state, &home] {
            fs::create_dir_all(path).unwrap();
        }
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
        cleanup::remove_fixture(&self.root).unwrap();
    }
}

/// `workspace-1`, which holds the test's repository, with the lead's Codex
/// pane and the reviewer's pane, and `workspace-2`, which `workspace list`
/// leaves out and which holds `{repo}-2`. The reviewer's session shows up
/// there as `pane-elsewhere` while the reviewer's pane is moved. The space
/// answers `workspace list` (no checkout stamp), `worktree list --cwd` for
/// the repository only, `worktree list --workspace`, and `pane list
/// --workspace`. It takes `agent prompt` to any pane, any `pane
/// report-metadata`, and any `notification show`.
///
/// The switches are the old executor's, and a read hook applies them to
/// the world before every read.
struct FinalSpace {
    space: StrictSpace,
    reviewer_host: Arc<Mutex<&'static str>>,
    /// Herdr no longer lists the reviewer's pane, as after it was closed.
    reviewer_gone: Arc<AtomicBool>,
    /// Herdr lists the reviewer's session in `pane-elsewhere` of workspace-2.
    reviewer_elsewhere: Arc<AtomicBool>,
    prompts: Arc<Mutex<Vec<(String, String)>>>,
    notifications: Arc<Mutex<Vec<String>>>,
}

/// The agent kind Herdr's word names.
fn agent_kind(word: &str) -> AgentKind {
    match word {
        "claude" => AgentKind::Known(AgentRuntime::ClaudeCode),
        "codex" => AgentKind::Known(AgentRuntime::Codex),
        other => AgentKind::Other(other.to_owned()),
    }
}

impl FinalSpace {
    fn new(worktree: &Path) -> Self {
        let mux = FakeMux::world()
            .endpoint("socket-a")
            .space("workspace-1")
            .checkout(worktree)
            .unstamped()
            .pane("pane-lead")
            .terminal("t1")
            .tab("tab-1")
            .codex()
            .session("session-lead")
            .idle()
            .pane("pane-reviewer")
            .terminal("t2")
            .tab("tab-2")
            .claude()
            .session("session-reviewer")
            .idle()
            .space("workspace-2")
            .checkout(format!("{}-2", worktree.display()))
            .unlisted()
            .pane("pane-elsewhere")
            .terminal("t2")
            .tab("tab-9")
            .claude()
            .session("session-reviewer")
            .working()
            .hidden()
            .build();
        for pane in ["pane-lead", "pane-reviewer", "pane-elsewhere"] {
            mux.answer(pane, InputOutcome::Delivered);
        }
        let reviewer_host = Arc::new(Mutex::new("claude"));
        let reviewer_gone = Arc::new(AtomicBool::new(false));
        let reviewer_elsewhere = Arc::new(AtomicBool::new(false));
        let (host, gone, elsewhere) = (
            Arc::clone(&reviewer_host),
            Arc::clone(&reviewer_gone),
            Arc::clone(&reviewer_elsewhere),
        );
        let repository = worktree.to_path_buf();
        mux.on_read(move |mux, read| {
            if let Read::Survey {
                scope: Surveyed::CheckoutOf(path),
                ..
            } = read
                && *path != repository
            {
                return Err(MuxError::Unavailable(format!(
                    "unexpected Herdr command: worktree list --cwd {}",
                    path.display()
                )));
            }
            mux.set_agent("pane-reviewer", Some(agent_kind(*host.lock().unwrap())));
            if gone.load(Ordering::Relaxed) {
                mux.hide_pane("pane-reviewer");
            } else {
                mux.show_pane("pane-reviewer");
            }
            if elsewhere.load(Ordering::Relaxed) {
                mux.show_pane("pane-elsewhere");
            } else {
                mux.hide_pane("pane-elsewhere");
            }
            Ok(())
        });
        let prompts = Arc::new(Mutex::new(Vec::new()));
        let typed = Arc::clone(&prompts);
        mux.on_input(move |_, attempt| {
            if let SentInput::Prompt(envelope) = &attempt.input {
                typed
                    .lock()
                    .unwrap()
                    .push((attempt.pane.as_str().to_owned(), envelope.clone()));
            }
        });
        let notifications = Arc::new(Mutex::new(Vec::new()));
        let shown = Arc::clone(&notifications);
        mux.on_apply(move |_, effect| {
            if let Effect::Notify { body, .. } = effect {
                shown.lock().unwrap().push(body.clone());
            }
            Ok(())
        });
        let takes = Takes::nothing()
            .input(|attempt| matches!(attempt.input, SentInput::Prompt(_)))
            .effect(|effect| {
                matches!(
                    effect,
                    Effect::Identify { .. } | Effect::Badges { .. } | Effect::Notify { .. }
                )
            });
        let mut listings = CHECKOUT_LISTINGS.to_vec();
        listings.push((ScopeKind::Space, Depth::Panes));
        Self {
            space: StrictSpace::new(mux, &listings, takes),
            reviewer_host,
            reviewer_gone,
            reviewer_elsewhere,
            prompts,
            notifications,
        }
    }

    fn envelope_for(&self, pane_id: &str) -> Value {
        let prompts = self.prompts.lock().unwrap();
        let (_, envelope) = prompts
            .iter()
            .rev()
            .find(|(pane, _)| pane == pane_id)
            .unwrap_or_else(|| panic!("no envelope was dispatched to {pane_id}"));
        delivered::envelope_of(envelope)
    }

    fn prompt_count(&self, pane_id: &str) -> usize {
        let prompts = self.prompts.lock().unwrap();
        prompts.iter().filter(|(pane, _)| pane == pane_id).count()
    }
}

struct FixedClock;
impl Clock for FixedClock {
    fn now_unix_ms(&self) -> u64 {
        42
    }
    fn wait_ms(&self, _: u64) {}
}

struct SequenceIds(AtomicU64);
impl IdGenerator for SequenceIds {
    fn next_id(&self, kind: &str) -> String {
        format!("{kind}-{}", self.0.fetch_add(1, Ordering::Relaxed))
    }
}

fn namespace() -> StateNamespace {
    StateNamespace::for_space("socket-a", "workspace-1")
}

struct Space {
    directory: TestDirectory,
    herdr: FinalSpace,
    ids: SequenceIds,
}

impl Space {
    fn new() -> Self {
        let directory = TestDirectory::new();
        let attribution = MutationAttribution::new(ActorKind::Human, "test", 1);
        StateStore::open(&directory.state)
            .unwrap()
            .transaction(|transaction| {
                transaction.activate_worktree(&namespace(), &attribution)?;
                for (alias, pane, session) in [
                    ("lead", "pane-lead", "session-lead"),
                    ("reviewer", "pane-reviewer", "session-reviewer"),
                ] {
                    let draft = PaneAddressDraft {
                        id: PaneAddressId::new(format!("address-{alias}")),
                        role_id: PaneRoleId::new(format!("role-{alias}")),
                        role_name: alias.to_owned(),
                        role_description: format!("Native collaborator addressed as @{alias}."),
                        alias: alias.to_owned(),
                    };
                    transaction.create_pane_address(&namespace(), &draft, &attribution)?;
                    transaction.bind_pane(
                        &namespace(),
                        &PaneBindingId::new(format!("binding-{alias}")),
                        &draft.id,
                        pane,
                        &attribution,
                    )?;
                    transaction.observe_agent_session(
                        &namespace(),
                        &format!("observation-{alias}"),
                        &draft.id,
                        &AgentSessionId::new(session),
                        "idle",
                        &attribution,
                    )?;
                }
                Ok(())
            })
            .unwrap();
        StateStore::open(&directory.state)
            .unwrap()
            .transaction(|transaction| transaction.complete_reconciliation(&namespace()))
            .unwrap();
        let herdr = FinalSpace::new(&directory.repo);
        let space = Self {
            directory,
            herdr,
            ids: SequenceIds(AtomicU64::new(1)),
        };
        space.register("address-lead", AgentRuntime::Codex, "pane-lead");
        space.register(
            "address-reviewer",
            AgentRuntime::ClaudeCode,
            "pane-reviewer",
        );
        space
    }

    /// The pane's current session completes the registration challenge, as
    /// onboarding has it do once the session is idle.
    fn register(&self, address: &str, host: AgentRuntime, pane: &str) {
        let runtime = self.runtime(pane);
        let challenge =
            issue_registration_challenge(&PaneAddressId::new(address), host, &runtime).unwrap();
        execute_integration_operation(
            "register",
            json!({"runtime":host,"protocol_version":"collab/v1","nonce":challenge.nonce,
                   "capabilities":complete_capability_profile()}),
            &runtime,
        )
        .unwrap();
    }

    fn runtime<'a>(
        &'a self,
        pane: &'a str,
    ) -> LifecycleRuntime<'a, FakeMux, FixedClock, SequenceIds> {
        LifecycleRuntime {
            state_directory: &self.directory.state,
            home_directory: Some(&self.directory.home),
            agent_config: namedrop::AgentConfigDirectories::default(),
            cwd: &self.directory.repo,
            workspace_id: None,
            current_pane_id: Some(pane),
            session_socket_id: "socket-a",
            mux: &self.herdr.space,
            clock: &FixedClock,
            ids: &self.ids,
        }
    }

    /// The prompt hook as the host runs it on a delivery turn: Claude Code
    /// names the turn by `prompt_id`, Codex by `turn_id`.
    fn prompt(&self, pane: &str, session: &str, turn: (&str, &str), prompt: &str) -> String {
        self.submitted(pane, session, turn, &self.as_typed(prompt))
    }

    fn submitted(&self, pane: &str, session: &str, turn: (&str, &str), prompt: &str) -> String {
        let mut input =
            json!({"hook_event_name":"UserPromptSubmit","prompt":prompt,"session_id":session});
        input[turn.0] = json!(turn.1);
        let output = execute_user_prompt_submit_hook(&input.to_string(), &self.runtime(pane))
            .expect("the delivery got no hook context");
        output["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn stop(&self, session: &str, turn: (&str, &str), text: &str) {
        self.stop_with(session, turn, text, false);
    }

    fn stop_with(&self, session: &str, turn: (&str, &str), text: &str, background_running: bool) {
        let stop = stop_facts(session, turn, text, background_running);
        settle_at_stop(&self.directory.state, "socket-a", &stop, 42, &mut |bound| {
            self.run(bound)
        });
    }

    /// A test holds a delivery as its envelope, but the host submits what was
    /// typed.
    fn as_typed(&self, prompt: &str) -> String {
        let typed = self.herdr.prompts.lock().unwrap();
        delivered::as_typed(typed.iter().map(|(_, typed)| typed.as_str()), prompt)
    }

    /// Lets a test show a second rendering of one delivery; production never
    /// attests twice.
    fn forget_presentation(&self) {
        rusqlite::Connection::open(self.directory.state.join("namedrop.sqlite3"))
            .unwrap()
            .execute(
                "UPDATE delivery_attempt_state SET pointer_presented_at_ms = NULL",
                [],
            )
            .unwrap();
    }

    /// What the Stop worker hands the CLI pipeline, run in-process.
    fn run(&self, bound: BoundOperation) -> Result<Value, String> {
        assert_eq!(bound.state, self.directory.state);
        assert_eq!(
            (bound.socket.as_str(), bound.workspace.as_str()),
            ("socket-a", "workspace-1")
        );
        let runtime = self.runtime(&bound.pane);
        match bound.operation {
            "reply" => dispatch::message(
                "reply",
                expand_attempt_reply(bound.input, Some(&bound.state))?,
                &runtime,
            ),
            operation => dispatch::integration(operation, bound.input, &runtime),
        }
    }

    /// The prompt hook's context with no session or turn id, which arms
    /// nothing: today's explicit commands.
    fn unkeyed_prompt(&self, pane: &str, prompt: &str) -> String {
        let prompt = self.as_typed(prompt);
        let input = json!({"hook_event_name":"UserPromptSubmit","prompt":prompt});
        let output = execute_user_prompt_submit_hook(&input.to_string(), &self.runtime(pane))
            .expect("the delivery got no hook context");
        output["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    fn rows(&self, sql: &str) -> Vec<Vec<String>> {
        let connection =
            rusqlite::Connection::open(self.directory.state.join("namedrop.sqlite3")).unwrap();
        let mut statement = connection.prepare(sql).unwrap();
        let width = statement.column_count();
        statement
            .query_map([], |row| {
                (0..width)
                    .map(|index| row.get::<_, String>(index))
                    .collect::<Result<Vec<_>, _>>()
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    }

    /// A message the person sends from the app, as the Channels composer
    /// does, then dispatched.
    fn send_as_person(&self, request_id: &str, source: &str, in_reply_to: Option<&str>) -> Value {
        let mut runtime = self.runtime("pane-lead");
        runtime.current_pane_id = None;
        runtime.workspace_id = Some("workspace-1");
        let sent = send_operator_message(
            request_id,
            source,
            in_reply_to.map(namedrop::MessageId::new).as_ref(),
            &runtime,
        )
        .unwrap();
        dispatch::wake(&runtime);
        sent
    }

    /// Every final message posted as the answer to the person's message.
    fn relayed(&self) -> Vec<Vec<String>> {
        self.rows(
            "SELECT message_id, thread_id, in_reply_to, body FROM collaboration_messages
             WHERE grammar_version='terminal-relay/v1' AND message_id LIKE 'terminal-relay:%'
             ORDER BY created_at_ms, rowid",
        )
    }

    fn dispositions(&self) -> Vec<Vec<String>> {
        self.rows("SELECT attempt_id, disposition_kind FROM delivery_dispositions ORDER BY occurred_at_ms, rowid")
    }

    fn claims(&self) -> Vec<Value> {
        let mut claims = Vec::new();
        let Ok(sessions) = fs::read_dir(self.directory.state.join("final-reply")) else {
            return claims;
        };
        for session in sessions {
            let file = session.unwrap().path().join("claims.json");
            claims.extend(serde_json::from_slice::<Vec<Value>>(&fs::read(file).unwrap()).unwrap());
        }
        claims
    }
}

fn stop_facts(
    session: &str,
    turn: (&str, &str),
    text: &str,
    background_running: bool,
) -> StopFacts {
    let (prompt_id, turn_id) = match turn.0 {
        "prompt_id" => (Some(turn.1), None),
        _ => (None, Some(turn.1)),
    };
    // Running work this turn started.
    let tasks = if background_running {
        vec!["background-1".to_owned()]
    } else {
        Vec::new()
    };
    StopFacts {
        session: session.to_owned(),
        turn: HostTurn::from_ids(prompt_id, turn_id),
        running_tasks: tasks.clone(),
        started_tasks: tasks,
        notified_tasks: Vec::new(),
        paused_tasks: Vec::new(),
        ended_tasks: Vec::new(),
        read_turn: true,
        text: FinalText::bounded(text),
    }
}

const CLAUDE: (&str, &str) = ("prompt_id", "reviewer-turn");
const CODEX: (&str, &str) = ("turn_id", "lead-turn");

#[test]
fn an_ask_and_its_answer_settle_from_final_messages_with_no_settle_command() {
    let space = Space::new();
    dispatch::message(
        "send",
        json!({"request_id":"ask","source":"@reviewer Reply with the single word pong.",
               "author_kind":"agent_session"}),
        &space.runtime("pane-lead"),
    )
    .unwrap();
    let ask = space.herdr.envelope_for("pane-reviewer");
    let ask_attempt = ask["control"]["attempt_id"].as_str().unwrap().to_owned();

    let context = space.prompt(
        "pane-reviewer",
        "session-reviewer",
        CLAUDE,
        &ask.to_string(),
    );
    assert!(
        context.starts_with("Namedrop verification:")
            && !context.contains("Settle only through the bound operations."),
        "{context}"
    );
    assert!(
        context.contains(
            "Namedrop commands: your final message is your reply to @lead. Namedrop sends it when your turn ends, so run no settle command and write no status line. If your turn ends while you wait on a helper, Namedrop sends nothing and you reply later. If it ends while background work you started this turn runs, Namedrop holds your final message, and your final message in the turn that work's notice starts is the reply. If the notice arrives during another turn, or another delivery reaches the turn it starts, answer with the line Namedrop shows. For no reply, run this line first."
        ),
        "{context}"
    );
    assert!(context.contains("\ncomplete.no-reply: "), "{context}");
    assert!(!context.contains("\nreply: "), "{context}");

    space.stop("session-reviewer", CLAUDE, "pong");
    let answer = space.herdr.envelope_for("pane-lead");
    assert_eq!(answer["untrusted_body"], "pong");
    let answer_attempt = answer["control"]["attempt_id"].as_str().unwrap().to_owned();

    let context = space.prompt("pane-lead", "session-lead", CODEX, &answer.to_string());
    assert!(
        context.contains(
            "Namedrop commands: this answers your message, so Namedrop closes it without a reply when your turn ends and your final message is for the person. To answer @reviewer instead, run this line."
        ),
        "{context}"
    );
    assert!(context.contains("\nreply: "), "{context}");
    assert!(!context.contains("complete.no-reply: "), "{context}");
    assert!(!context.contains("If your turn ends while"), "{context}");

    space.stop("session-lead", CODEX, "@reviewer answered: pong.");
    assert_eq!(
        space.dispositions(),
        [
            vec![ask_attempt.clone(), "reply".to_owned()],
            vec![answer_attempt.clone(), "no_reply".to_owned()],
        ]
    );
    assert_eq!(
        space
            .rows("SELECT message_id FROM collaboration_messages")
            .len(),
        2
    );
    assert_eq!(
        space.herdr.prompt_count("pane-reviewer"),
        1,
        "the answer looped back"
    );
    let states: Vec<_> = space
        .claims()
        .iter()
        .map(|claim| {
            (
                claim["attempt_id"].as_str().unwrap().to_owned(),
                claim["state"].clone(),
            )
        })
        .collect();
    assert!(
        states.contains(&(ask_attempt, json!({"done":"replied"}))),
        "{states:?}"
    );
    assert!(
        states.contains(&(answer_attempt, json!({"done":"no_replied"}))),
        "{states:?}"
    );
}

#[test]
fn an_explicit_settle_wins_and_a_prompt_without_turn_ids_keeps_todays_commands() {
    let space = Space::new();
    dispatch::message(
        "send",
        json!({"request_id":"ask","source":"@reviewer Is the login path safe?",
               "author_kind":"agent_session"}),
        &space.runtime("pane-lead"),
    )
    .unwrap();
    let ask = space.herdr.envelope_for("pane-reviewer");
    let attempt = ask["control"]["attempt_id"].as_str().unwrap().to_owned();

    let unkeyed = execute_user_prompt_submit_hook(
        &json!({"hook_event_name":"UserPromptSubmit","prompt":space.as_typed(&ask.to_string())})
            .to_string(),
        &space.runtime("pane-reviewer"),
    )
    .unwrap();
    space.forget_presentation();
    let unkeyed = unkeyed["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(
        unkeyed.contains("Settle only through the bound operations."),
        "{unkeyed}"
    );
    assert!(
        unkeyed.contains("Namedrop commands: the runnable lines this delivery authorized"),
        "{unkeyed}"
    );
    assert!(
        !space.directory.state.join("final-reply").exists(),
        "an unkeyed prompt armed a claim"
    );

    space.prompt(
        "pane-reviewer",
        "session-reviewer",
        CLAUDE,
        &ask.to_string(),
    );
    dispatch::integration(
        "complete",
        json!({"attempt_id": attempt}),
        &space.runtime("pane-reviewer"),
    )
    .unwrap();
    space.stop("session-reviewer", CLAUDE, "No reply needed; I noted it.");
    assert_eq!(space.dispositions(), [vec![attempt, "no_reply".to_owned()]]);
    assert_eq!(
        space
            .rows("SELECT message_id FROM collaboration_messages")
            .len(),
        1
    );
    assert_eq!(space.claims()[0]["state"], json!({"done":"superseded"}));
}

#[test]
fn a_prompt_from_another_session_or_a_later_turn_never_settles_the_delivery() {
    let space = Space::new();
    dispatch::message(
        "send",
        json!({"request_id":"ask","source":"@reviewer Reply with the single word pong.",
               "author_kind":"agent_session"}),
        &space.runtime("pane-lead"),
    )
    .unwrap();
    let ask = space.herdr.envelope_for("pane-reviewer").to_string();

    let other = space.prompt("pane-reviewer", "session-stale", CLAUDE, &ask);
    assert!(
        other.contains("Settle only through the bound operations."),
        "{other}"
    );

    space.forget_presentation();
    space.prompt("pane-reviewer", "session-reviewer", CLAUDE, &ask);
    space.stop("session-reviewer", ("prompt_id", "a-later-turn"), "pong");
    assert!(space.dispositions().is_empty());
    assert_eq!(space.herdr.prompt_count("pane-lead"), 0);
    assert_eq!(space.claims()[0]["state"], json!({"left":"interrupted"}));
}

#[test]
fn a_typed_mention_round_trip_shows_the_answer_once_and_closes_without_a_loop() {
    let space = Space::new();
    let sent = execute_user_prompt_submit_hook(
        &json!({"hook_event_name":"UserPromptSubmit","prompt":"@reviewer Reply with the single word pong.",
                "session_id":"session-lead","turn_id":"typed"})
        .to_string(),
        &space.runtime("pane-lead"),
    )
    .unwrap();
    assert_eq!(sent["reason"], "Collaboration message sent.");
    dispatch::wake(&space.runtime("pane-lead"));
    let ask = space.herdr.envelope_for("pane-reviewer");
    assert_eq!(ask["control"]["reply_mode"], "return_to_source");

    space.prompt(
        "pane-reviewer",
        "session-reviewer",
        CLAUDE,
        &ask.to_string(),
    );
    space.stop("session-reviewer", CLAUDE, "pong");
    let answer = space.herdr.envelope_for("pane-lead");
    assert_eq!(answer["control"]["reply_mode"], "report_to_human");
    assert_eq!(answer["untrusted_body"], "pong");

    let context = space.prompt("pane-lead", "session-lead", CODEX, &answer.to_string());
    assert!(
        context.ends_with(
            "\n\nNamedrop commands: none. Namedrop closes this without a reply when your turn ends; show it to the person once in your final message."
        ),
        "{context}"
    );
    space.stop("session-lead", CODEX, "@reviewer says: pong");
    let kinds: Vec<_> = space
        .dispositions()
        .into_iter()
        .map(|row| row[1].clone())
        .collect();
    assert_eq!(kinds, ["reply", "no_reply"]);
    assert_eq!(
        space
            .rows("SELECT message_id FROM collaboration_messages")
            .len(),
        2
    );
}

#[test]
fn background_work_at_the_stop_leaves_the_reply_for_the_agent_to_settle() {
    let space = Space::new();
    dispatch::message(
        "send",
        json!({"request_id":"ask","source":"@reviewer Run the build and report.",
               "author_kind":"agent_session"}),
        &space.runtime("pane-lead"),
    )
    .unwrap();
    let ask = space.herdr.envelope_for("pane-reviewer");
    let attempt = ask["control"]["attempt_id"].as_str().unwrap().to_owned();
    space.prompt(
        "pane-reviewer",
        "session-reviewer",
        CLAUDE,
        &ask.to_string(),
    );
    space.stop_with(
        "session-reviewer",
        CLAUDE,
        "Started the build in the background.",
        true,
    );
    assert_eq!(space.claims()[0]["state"], json!({"left":"held"}));

    // A background agent's report arrives as a prompt no field tells apart
    // from the person's, so its turn settles nothing on its own.
    let continuation = ("prompt_id", "continuation-turn");
    execute_user_prompt_submit_hook(
        &json!({"hook_event_name":"UserPromptSubmit","session_id":"session-reviewer",
                "prompt_id":continuation.1,
                "prompt":"<system-reminder>\nA background agent finished.\n</system-reminder>"})
        .to_string(),
        &space.runtime("pane-reviewer"),
    );
    space.stop("session-reviewer", continuation, "The build passed.");
    assert!(space.dispositions().is_empty());
    assert_eq!(space.herdr.prompt_count("pane-lead"), 0);
    let inbox =
        execute_integration_operation("delivery.inbox", json!({}), &space.runtime("pane-reviewer"))
            .unwrap();
    assert_eq!(
        inbox["envelopes"][0]["control"]["attempt_id"], attempt,
        "a held delivery left the inbox"
    );

    let reply = ask["control"]["tool"]["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|operation| operation["token"] == "reply")
        .unwrap();
    let mut input = reply["input"].clone();
    input["source"] = json!("@lead The build passed.");
    dispatch::message(
        "reply",
        expand_attempt_reply(input, Some(&space.directory.state)).unwrap(),
        &space.runtime("pane-reviewer"),
    )
    .unwrap();
    assert_eq!(space.dispositions(), [vec![attempt, "reply".to_owned()]]);
    assert_eq!(
        space.herdr.envelope_for("pane-lead")["untrusted_body"],
        "The build passed."
    );
}

/// The armed text shows one line. A delivery whose explicit block shows any
/// other line asks the agent to run it, so it keeps that block as it is.
#[test]
fn a_delivery_that_shows_another_line_keeps_todays_commands() {
    let space = Space::new();
    let keeps_todays_commands = |shape: &str,
                                 (pane, session, turn): (&str, &str, (&str, &str)),
                                 envelope: &Value,
                                 line: &str| {
        let typed = space.as_typed(&envelope.to_string());
        let today = space.unkeyed_prompt(pane, &typed);
        assert!(
            today.contains("Namedrop commands: the runnable lines this delivery authorized")
                && today.contains(&format!("\n{line}: ")),
            "{shape}: {today}"
        );
        space.forget_presentation();
        assert_eq!(
            space.submitted(pane, session, turn, &typed),
            today,
            "{shape}"
        );
    };
    dispatch::message(
        "send",
        json!({"request_id":"ask","source":"@reviewer Review the attached context.",
               "author_kind":"agent_session",
               "context":{"capture_git_state":false,"snapshot":{
                   "objective":"review the login path","applicable_user_instructions":[],
                   "decisions":[],"completed_work":[],"current_work":[],"blockers":[],
                   "evidence":[]}}}),
        &space.runtime("pane-lead"),
    )
    .unwrap();
    let ask = space.herdr.envelope_for("pane-reviewer");
    assert!(ask["control"]["attached_context"].is_object(), "{ask}");
    keeps_todays_commands(
        "attached context",
        ("pane-reviewer", "session-reviewer", CLAUDE),
        &ask,
        "context.read",
    );

    let reply = ask["control"]["tool"]["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|operation| operation["token"] == "reply")
        .unwrap();
    let mut input = reply["input"].clone();
    input["source"] = json!("@lead The login path is safe.");
    dispatch::message(
        "reply",
        expand_attempt_reply(input, Some(&space.directory.state)).unwrap(),
        &space.runtime("pane-reviewer"),
    )
    .unwrap();
    let answer = space.herdr.envelope_for("pane-lead");
    assert!(
        answer["control"]["review_source_message_id"].is_string(),
        "{answer}"
    );
    keeps_todays_commands(
        "review source",
        ("pane-lead", "session-lead", CODEX),
        &answer,
        "handoff.results",
    );
    assert!(
        !space.directory.state.join("final-reply").exists(),
        "a delivery with another line armed a claim"
    );
}

/// One installed hook the host started, and the descriptor whose end of file
/// means the hook and any worker it detached have exited.
struct RunningHook {
    child: std::process::Child,
    done: std::os::unix::net::UnixStream,
}

impl RunningHook {
    /// What the hook printed, once the hook itself has exited. A worker it
    /// detached may still run.
    fn output(&mut self) -> String {
        let mut output = String::new();
        if let Some(mut stdout) = self.child.stdout.take() {
            std::io::Read::read_to_string(&mut stdout, &mut output).unwrap();
        }
        assert!(self.child.wait().unwrap().success());
        output
    }

    fn wait(mut self) {
        self.output();
        // macOS refuses a socket option with EINVAL once the peer has closed, and
        // then the read below returns end of file at once.
        if let Err(error) = self
            .done
            .set_read_timeout(Some(std::time::Duration::from_secs(15)))
        {
            assert_eq!(error.raw_os_error(), Some(libc::EINVAL), "{error}");
        }
        assert_eq!(
            std::io::Read::read(&mut self.done, &mut [0]).unwrap(),
            0,
            "the hook's worker outlived its deadline"
        );
    }
}

impl Space {
    /// A Herdr that answers like the space and logs every prompt it is
    /// handed, for the binary's own processes.
    fn herdr_script(&self) -> PathBuf {
        let herdr = self.directory.root.join("herdr");
        executable::write_executable(
            &herdr,
            format!(
                r#"#!/usr/bin/env python3
import json, sys, time
from pathlib import Path
if (Path(__file__).parent / 'herdr-slow').exists(): time.sleep(6)
args = sys.argv[1:]
if args[:1] == ['--socket']: args = args[2:]
here = Path(__file__).parent
reviewer = here / 'reviewer-status'
panes = {{'result': {{'panes': [
    {{'pane_id':'pane-lead','terminal_id':'t1','tab_id':'tab-1','workspace_id':'workspace-1','agent':'codex','agent_session':{{'value':'session-lead'}},'agent_status':'idle'}},
    {{'pane_id':'pane-reviewer','terminal_id':'t2','tab_id':'tab-2','workspace_id':'workspace-1','agent':'claude','agent_session':{{'value':'session-reviewer'}},'agent_status':reviewer.read_text() if reviewer.exists() else 'idle'}}]}}}}
if args[:2] == ['workspace','list']: print(json.dumps({{'result':{{'workspaces':[{{'workspace_id':'workspace-1'}}]}}}}))
elif args[:2] == ['worktree','list']: print(json.dumps({{'result':{{'worktrees':[{{'path':{repo:?},'open_workspace_id':'workspace-1'}}]}}}}))
elif args[:2] == ['pane','list']: print(json.dumps(panes))
elif args[:3] == ['pane','read','pane-reviewer'] and (here / 'reviewer-screen').exists(): print((here / 'reviewer-screen').read_text(), end='')
elif args[:2] == ['agent','send-keys']:
    with (here / 'herdr-keys').open('a') as log: log.write(json.dumps(args[2:]) + '\n')
    cleared = here / 'reviewer-screen-cleared'
    if args[2] == 'pane-reviewer' and set(args[3:]) == {{'ctrl+u'}} and cleared.exists(): (here / 'reviewer-screen').write_text(cleared.read_text())
    print('{{}}')
else:
    if args[:2] == ['agent','prompt']:
        with (Path(__file__).parent / 'herdr-prompts').open('a') as log: log.write(json.dumps(args[2:]) + '\n')
    print('{{}}')
"#,
                repo = self.directory.repo.display().to_string()
            ),
        );
        herdr
    }

    /// The binary as a host runs it, against the script Herdr.
    fn command(&self) -> Command {
        self.hosted(env!("CARGO_BIN_EXE_namedrop"))
    }

    /// `program` in the environment a host gives the binary.
    fn hosted(&self, program: &str) -> Command {
        let root = &self.directory.root;
        let mut command = Command::new(program);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &self.directory.home)
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("HERDR_SOCKET_PATH", "socket-a")
            .env("HERDR_BIN_PATH", self.herdr_script())
            .current_dir(&self.directory.repo);
        command
    }

    /// The envelopes the binary's own processes prompted into `pane`.
    fn prompted_by_binary(&self, pane: &str) -> Vec<Value> {
        let Ok(log) = fs::read_to_string(self.directory.root.join("herdr-prompts")) else {
            return Vec::new();
        };
        log.lines()
            .map(|line| serde_json::from_str::<Vec<String>>(line).unwrap())
            .filter(|args| args[0] == pane)
            .map(|args| delivered::envelope_of(&args[1]))
            .collect()
    }

    /// The installed Stop hook as the host runs it. It returns once the hook
    /// and any detached worker have exited.
    fn installed_stop(&self, session: &str, prompt_id: &str, text: &str) {
        self.start_installed_stop(session, prompt_id, text).wait();
    }

    fn start_installed_stop(&self, session: &str, prompt_id: &str, text: &str) -> RunningHook {
        self.start_installed_stop_with(json!({"hook_event_name":"Stop","session_id":session,
            "prompt_id":prompt_id,"last_assistant_message":text,"background_tasks":[]}))
    }

    fn start_installed_stop_with(&self, payload: Value) -> RunningHook {
        self.start_installed_stop_in(self.command(), payload)
    }

    /// The Stop hook run from `command`, which a test may give the pane's
    /// workspace as Herdr does.
    fn start_installed_stop_in(&self, mut command: Command, payload: Value) -> RunningHook {
        command.env("NAMEDROP_HERDR_TIMEOUT_MS", "250");
        self.start_installed_hook_in(command, "stop", payload)
    }

    /// The installed `hook` run from `command` with the host's `payload`.
    fn start_installed_hook_in(
        &self,
        mut command: Command,
        hook: &str,
        payload: Value,
    ) -> RunningHook {
        use std::os::{fd::AsRawFd, unix::process::CommandExt};
        let (done, lifetime) = std::os::unix::net::UnixStream::pair().unwrap();
        let lifetime_fd = lifetime.as_raw_fd();
        // The hook and its detached worker inherit this descriptor, so its
        // end of file means both have exited.
        unsafe {
            command.pre_exec(move || {
                let flags = libc::fcntl(lifetime_fd, libc::F_GETFD);
                if flags == -1
                    || libc::fcntl(lifetime_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command
            .args(["hook", hook, "--state-directory"])
            .arg(&self.directory.state)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        drop(lifetime);
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        RunningHook { child, done }
    }
}

/// The installed Stop hook and its detached worker run the same CLI pipeline
/// as the agent's own bound line, against a Herdr that answers like the space.
#[test]
fn the_installed_stop_hook_settles_through_the_cli_pipeline() {
    let space = Space::new();
    dispatch::message(
        "send",
        json!({"request_id":"ask","source":"@reviewer Reply with the single word pong.",
               "author_kind":"agent_session"}),
        &space.runtime("pane-lead"),
    )
    .unwrap();
    let ask = space.herdr.envelope_for("pane-reviewer");
    let attempt = ask["control"]["attempt_id"].as_str().unwrap().to_owned();
    space.prompt(
        "pane-reviewer",
        "session-reviewer",
        CLAUDE,
        &ask.to_string(),
    );
    space.installed_stop("session-reviewer", CLAUDE.1, "pong");
    assert_eq!(space.dispositions(), [vec![attempt, "reply".to_owned()]]);
    assert_eq!(
        space.rows("SELECT body FROM collaboration_messages ORDER BY created_at_ms DESC, rowid DESC LIMIT 1"),
        [vec!["pong".to_owned()]]
    );
    assert_eq!(space.claims()[0]["state"], json!({"done":"replied"}));
}

/// The bound reply of `envelope` with `source` as its text, as the agent
/// runs it.
fn bound_reply(space: &Space, envelope: &Value, source: &str) -> Value {
    let reply = envelope["control"]["tool"]["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|operation| operation["token"] == "reply")
        .unwrap();
    let mut input = reply["input"].clone();
    input["source"] = json!(source);
    expand_attempt_reply(input, Some(&space.directory.state)).unwrap()
}

/// A delivery Namedrop closed without a reply at the end of a turn still
/// takes the agent's later reply, in the same thread; one the agent closed
/// itself refuses a reply in a plain sentence.
#[test]
fn a_reply_after_namedrop_closed_the_delivery_follows_up_in_its_thread() {
    let space = Space::new();
    dispatch::message(
        "send",
        json!({"request_id":"ask","source":"@reviewer Reply with the single word pong.",
               "author_kind":"agent_session"}),
        &space.runtime("pane-lead"),
    )
    .unwrap();
    let ask = space.herdr.envelope_for("pane-reviewer");
    space.prompt(
        "pane-reviewer",
        "session-reviewer",
        CLAUDE,
        &ask.to_string(),
    );
    space.stop("session-reviewer", CLAUDE, "pong");
    let answer = space.herdr.envelope_for("pane-lead");
    let attempt = answer["control"]["attempt_id"].as_str().unwrap().to_owned();
    space.prompt("pane-lead", "session-lead", CODEX, &answer.to_string());
    space.stop("session-lead", CODEX, "@reviewer answered: pong.");
    assert_eq!(
        space.rows(&format!(
            "SELECT disposition_id, disposition_kind FROM delivery_dispositions WHERE attempt_id='{attempt}'"
        )),
        [vec![format!("final-reply-no-reply:{attempt}"), "no_reply".to_owned()]]
    );

    let followed = dispatch::message(
        "reply",
        bound_reply(&space, &answer, "@reviewer Thanks, one more question."),
        &space.runtime("pane-lead"),
    )
    .unwrap();
    assert_eq!(followed["attempt_id"], json!(attempt));
    assert_eq!(followed["disposition"], "no_reply", "{followed}");
    assert_eq!(
        space.rows(&format!(
            "SELECT CAST(m.in_reply_to = d.message_id AS TEXT), CAST(m.thread_id = o.thread_id AS TEXT)
             FROM collaboration_messages m, delivery_attempts a
             JOIN mailbox_deliveries d USING (session_socket_id, worktree_id, delivery_id, address_id)
             JOIN collaboration_messages o ON o.message_id = d.message_id
             WHERE a.attempt_id = '{attempt}' AND m.message_id = '{}'",
            followed["message_id"].as_str().unwrap()
        ))
        .iter()
        .map(|row| row.join(","))
        .collect::<Vec<_>>(),
        ["1,1"]
    );
    assert_eq!(
        space.rows(&format!(
            "SELECT disposition_kind FROM delivery_dispositions WHERE attempt_id='{attempt}'"
        )),
        [vec!["no_reply".to_owned()]]
    );
    let second = space.herdr.envelope_for("pane-reviewer");
    assert_eq!(second["untrusted_body"], "Thanks, one more question.");
    dispatch::integration(
        "complete",
        json!({"attempt_id": second["control"]["attempt_id"]}),
        &space.runtime("pane-reviewer"),
    )
    .unwrap();
    let refused = dispatch::message(
        "reply",
        bound_reply(&space, &second, "@lead It is green."),
        &space.runtime("pane-reviewer"),
    )
    .unwrap_err();
    assert!(
        refused.contains("this delivery is already closed without a reply")
            && !refused.contains("UNIQUE"),
        "{refused}"
    );
}

/// A message from the person is answered by the agent's final message, posted
/// once in the thread it came from, in reply to it, as the terminal relay
/// posts it; a thread reply stays in its thread, and the agent can still
/// follow up with its bound reply.
#[test]
fn a_message_from_the_person_is_answered_in_its_thread_by_the_final_message() {
    let space = Space::new();
    let question = space.send_as_person("question", "@reviewer Option A or option B?", None);
    let ask = space.herdr.envelope_for("pane-reviewer");
    let attempt = ask["control"]["attempt_id"].as_str().unwrap().to_owned();
    let context = space.prompt(
        "pane-reviewer",
        "session-reviewer",
        CLAUDE,
        &ask.to_string(),
    );
    assert!(
        context.ends_with(
            "\n\nNamedrop commands: your final message goes to the person when your turn ends, so run no settle command and write no status line. If your turn ends while you wait on a helper, Namedrop sends nothing and you reply later. If it ends while background work you started this turn runs, Namedrop holds your final message, and your final message in the turn that work's notice starts is the reply. If the notice arrives during another turn, or another delivery reaches the turn it starts, answer with the line Namedrop shows."
        ),
        "{context}"
    );
    assert!(
        !context.contains("Settle only through the bound operations."),
        "{context}"
    );

    space.stop("session-reviewer", CLAUDE, "Option B.");
    let thread = question["thread_id"].as_str().unwrap().to_owned();
    let first = format!("terminal-relay:{attempt}");
    assert_eq!(
        space.relayed(),
        [vec![
            first.clone(),
            thread.clone(),
            question["message_id"].as_str().unwrap().to_owned(),
            "Option B.".to_owned()
        ]]
    );
    assert_eq!(
        space.rows("SELECT disposition_id, disposition_kind FROM delivery_dispositions"),
        [vec![
            format!("terminal-relay-no-reply:{attempt}"),
            "no_reply".to_owned()
        ]]
    );
    assert_eq!(space.claims()[0]["state"], json!({"done":"no_replied"}));

    let why = space.send_as_person("why", "@reviewer Why B?", Some(&first));
    assert_eq!(why["thread_id"], json!(thread));
    let followup = space.herdr.envelope_for("pane-reviewer");
    let second = followup["control"]["attempt_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(second, attempt);
    let turn = ("prompt_id", "why-turn");
    space.prompt(
        "pane-reviewer",
        "session-reviewer",
        turn,
        &followup.to_string(),
    );
    space.stop("session-reviewer", turn, "Because it is faster.");
    let relayed = space.relayed();
    assert_eq!(relayed.len(), 2, "{relayed:?}");
    assert_eq!(
        relayed[1],
        [
            format!("terminal-relay:{second}"),
            thread.clone(),
            why["message_id"].as_str().unwrap().to_owned(),
            "Because it is faster.".to_owned()
        ]
    );

    let more = dispatch::message(
        "reply",
        bound_reply(&space, &followup, "@operator It also uses less memory."),
        &space.runtime("pane-reviewer"),
    )
    .unwrap();
    assert_eq!(more["thread_id"], json!(thread));
    assert_eq!(more["disposition"], "no_reply", "{more}");
    assert_eq!(
        space.rows(&format!(
            "SELECT in_reply_to FROM collaboration_messages WHERE message_id='{}'",
            more["message_id"].as_str().unwrap()
        )),
        [vec![why["message_id"].as_str().unwrap().to_owned()]]
    );
}

/// An agent that closes the person's message itself has said there is no
/// answer to post, so its final message stays in the pane.
#[test]
fn an_explicit_no_reply_to_the_person_posts_no_final_message() {
    let space = Space::new();
    space.send_as_person("question", "@reviewer Thanks, that is all.", None);
    let ask = space.herdr.envelope_for("pane-reviewer");
    let attempt = ask["control"]["attempt_id"].as_str().unwrap().to_owned();
    space.prompt(
        "pane-reviewer",
        "session-reviewer",
        CLAUDE,
        &ask.to_string(),
    );
    execute_integration_operation(
        "complete",
        json!({"attempt_id": attempt}),
        &space.runtime("pane-reviewer"),
    )
    .unwrap();
    space.stop(
        "session-reviewer",
        CLAUDE,
        "Closed the delivery with no reply.",
    );
    assert!(space.relayed().is_empty(), "{:?}", space.relayed());
    assert_eq!(space.dispositions(), [vec![attempt, "no_reply".to_owned()]]);
    assert_eq!(space.claims()[0]["state"], json!({"done":"superseded"}));
}

/// What the person types into the pane through Live Peek is a plain prompt
/// with no envelope: it arms nothing, so its turn's final message is never
/// posted anywhere, even while a message from the person is outstanding.
#[test]
fn a_prompt_typed_into_the_pane_is_never_relayed() {
    let space = Space::new();
    space.send_as_person("question", "@reviewer Option A or option B?", None);
    let ask = space.herdr.envelope_for("pane-reviewer");
    let attempt = ask["control"]["attempt_id"].as_str().unwrap().to_owned();
    execute_user_prompt_submit_hook(
        &json!({"hook_event_name":"UserPromptSubmit","session_id":"session-reviewer",
                "prompt_id":"typed-turn","prompt":"Which file did you change?"})
        .to_string(),
        &space.runtime("pane-reviewer"),
    );
    space.installed_stop("session-reviewer", "typed-turn", "I changed src/main.rs.");
    assert!(space.relayed().is_empty(), "{:?}", space.relayed());
    assert!(space.dispositions().is_empty());
    assert!(!space.directory.state.join("final-reply").exists());
    let inbox =
        execute_integration_operation("delivery.inbox", json!({}), &space.runtime("pane-reviewer"))
            .unwrap();
    assert_eq!(inbox["envelopes"][0]["control"]["attempt_id"], attempt);
}

/// A turn that ends while the agent waits on a helper it asked during the
/// turn ends with a status, not the answer: Namedrop posts nothing and the
/// delivery stays open. Once the helper has answered, the final message is
/// the answer again.
#[test]
fn a_turn_that_ends_waiting_on_a_helper_holds_the_delivery() {
    for answered in [false, true] {
        let space = Space::new();
        space.send_as_person(
            "question",
            "@reviewer Is option B safe? Ask the lead.",
            None,
        );
        let ask = space.herdr.envelope_for("pane-reviewer");
        let attempt = ask["control"]["attempt_id"].as_str().unwrap().to_owned();
        space.prompt(
            "pane-reviewer",
            "session-reviewer",
            CLAUDE,
            &ask.to_string(),
        );
        dispatch::message(
            "send",
            json!({"request_id":"helper","source":"@lead Is option B safe?",
                   "author_kind":"agent_session"}),
            &space.runtime("pane-reviewer"),
        )
        .unwrap();
        if answered {
            let helper = space.herdr.envelope_for("pane-lead");
            dispatch::message(
                "reply",
                bound_reply(&space, &helper, "@reviewer Yes, it is safe."),
                &space.runtime("pane-lead"),
            )
            .unwrap();
        }
        space.stop(
            "session-reviewer",
            CLAUDE,
            "I asked @lead and will answer when it replies.",
        );
        if answered {
            assert_eq!(space.relayed().len(), 1, "{answered}");
            continue;
        }
        assert!(space.relayed().is_empty(), "{:?}", space.relayed());
        assert!(space.dispositions().is_empty());
        assert_eq!(space.claims()[0]["state"], json!({"left":"held"}));
        let inbox = execute_integration_operation(
            "delivery.inbox",
            json!({}),
            &space.runtime("pane-reviewer"),
        )
        .unwrap();
        assert!(
            inbox["envelopes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|envelope| envelope["control"]["attempt_id"] == attempt.as_str()),
            "a held delivery left the inbox"
        );

        // Every later turn says how to answer it: the helper's answer, and
        // a prompt typed into the pane alike.
        let way_back = |context: &str| {
            let (head, line) = context.rsplit_once('\n').unwrap();
            head.ends_with(&format!(
                "\n\nNamedrop: delivery {attempt} from an earlier turn is still open; to answer it, run this line with your text in place of <reply body>."
            )) && line.starts_with("reply: ")
                && line.contains(&format!(r#""attempt_id":"{attempt}""#))
        };
        // The agent going idle settles the attempt's state without a
        // disposition, as reconciliation does, so the mailbox moves on.
        StateStore::open(&space.directory.state)
            .unwrap()
            .transaction(|transaction| {
                transaction.transition_delivery_attempt(
                    &namespace(),
                    &namedrop::DeliveryAttemptId::new(&attempt),
                    namedrop::DeliveryState::Settled,
                    &MutationAttribution::new(ActorKind::Plugin, "test", 42),
                )
            })
            .unwrap();
        let helper = space.herdr.envelope_for("pane-lead");
        dispatch::message(
            "reply",
            bound_reply(&space, &helper, "@reviewer Yes, it is safe."),
            &space.runtime("pane-lead"),
        )
        .unwrap();
        let answer = space.herdr.envelope_for("pane-reviewer");
        assert_eq!(answer["untrusted_body"], "Yes, it is safe.");
        let turn = ("prompt_id", "answer-turn");
        let context = space.prompt(
            "pane-reviewer",
            "session-reviewer",
            turn,
            &answer.to_string(),
        );
        assert!(way_back(&context), "{context}");
        space.stop("session-reviewer", turn, "The lead says option B is safe.");
        let typed = space.prompt(
            "pane-reviewer",
            "session-reviewer",
            ("prompt_id", "typed-turn"),
            "Did you hear back?",
        );
        assert!(way_back(&typed), "{typed}");

        let answered = dispatch::message(
            "reply",
            bound_reply(
                &space,
                &ask,
                "@operator Yes: the lead says option B is safe.",
            ),
            &space.runtime("pane-reviewer"),
        )
        .unwrap();
        assert_eq!(answered["disposition"], "reply");
        let after = execute_user_prompt_submit_hook(
            &json!({"hook_event_name":"UserPromptSubmit","session_id":"session-reviewer",
                    "prompt_id":"after-turn","prompt":"Thanks."})
            .to_string(),
            &space.runtime("pane-reviewer"),
        );
        let after = after
            .as_ref()
            .and_then(|output| output["hookSpecificOutput"]["additionalContext"].as_str())
            .unwrap_or_default();
        assert!(!after.contains("is still open"), "{after}");
    }

    let space = Space::new();
    space.send_as_person("question", "@reviewer Run the build and report.", None);
    let ask = space.herdr.envelope_for("pane-reviewer");
    space.prompt(
        "pane-reviewer",
        "session-reviewer",
        CLAUDE,
        &ask.to_string(),
    );
    space.stop_with("session-reviewer", CLAUDE, "The build is running.", true);
    assert!(space.relayed().is_empty());
    assert_eq!(space.claims()[0]["state"], json!({"left":"held"}));
}

/// A reply the agent sends during the turn opens no request, even to an
/// agent who has not answered it: a late answer to an earlier delivery does
/// not make the turn wait, so the person still gets the final message.
#[test]
fn a_reply_to_an_earlier_delivery_does_not_hold_the_turn() {
    let space = Space::new();
    dispatch::message(
        "send",
        json!({"request_id":"earlier","source":"@reviewer Which branch is the release on?",
               "author_kind":"agent_session"}),
        &space.runtime("pane-lead"),
    )
    .unwrap();
    let earlier = space.herdr.envelope_for("pane-reviewer");
    StateStore::open(&space.directory.state)
        .unwrap()
        .transaction(|transaction| {
            transaction.transition_delivery_attempt(
                &namespace(),
                &namedrop::DeliveryAttemptId::new(
                    earlier["control"]["attempt_id"].as_str().unwrap(),
                ),
                namedrop::DeliveryState::Settled,
                &MutationAttribution::new(ActorKind::Plugin, "test", 42),
            )
        })
        .unwrap();
    space.send_as_person("question", "@reviewer Is option B safe?", None);
    let question = space.herdr.envelope_for("pane-reviewer");
    assert_ne!(
        question["control"]["attempt_id"],
        earlier["control"]["attempt_id"]
    );
    space.prompt(
        "pane-reviewer",
        "session-reviewer",
        CLAUDE,
        &question.to_string(),
    );
    dispatch::message(
        "reply",
        bound_reply(&space, &earlier, "@lead It is on release-2."),
        &space.runtime("pane-reviewer"),
    )
    .unwrap();
    space.stop("session-reviewer", CLAUDE, "Yes, option B is safe.");
    assert_eq!(
        space
            .relayed()
            .iter()
            .map(|row| row[3].as_str())
            .collect::<Vec<_>>(),
        ["Yes, option B is safe."]
    );
    assert_eq!(space.claims()[0]["state"], json!({"done":"no_replied"}));
}

/// A reply the Stop settles is an agent reply like any other: it spends the
/// thread's Automatic Reply Limit, and the reply that reaches it pauses the
/// thread and holds the answer until the person continues.
#[test]
fn stop_settled_replies_count_against_the_automatic_reply_limit() {
    let space = Space::new();
    dispatch::message(
        "send",
        json!({"request_id":"ask","source":"@reviewer Reply with the single word pong.",
               "author_kind":"agent_session"}),
        &space.runtime("pane-lead"),
    )
    .unwrap();
    let ask = space.herdr.envelope_for("pane-reviewer");
    let thread = space.rows("SELECT thread_id FROM collaboration_messages WHERE request_id='ask'")
        [0][0]
        .clone();
    rusqlite::Connection::open(space.directory.state.join("namedrop.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE thread_controls SET reply_allowance=1 WHERE thread_id=?1",
            [&thread],
        )
        .unwrap();
    space.prompt(
        "pane-reviewer",
        "session-reviewer",
        CLAUDE,
        &ask.to_string(),
    );
    space.stop("session-reviewer", CLAUDE, "pong");
    assert_eq!(space.dispositions()[0][1], "reply");
    assert_eq!(
        space.rows(&format!(
            "SELECT CAST(paused AS TEXT), CAST(reply_allowance AS TEXT) FROM thread_controls
             WHERE thread_id='{thread}'"
        )),
        [vec!["1".to_owned(), "0".to_owned()]]
    );
    assert_eq!(
        space.rows(&format!(
            "SELECT source_kind FROM attention_items
             WHERE source_id='{thread}' AND resolved_at_ms IS NULL"
        )),
        [vec!["automatic_reply_limit".to_owned()]]
    );
    assert_eq!(
        space.herdr.prompt_count("pane-lead"),
        0,
        "the paused thread delivered the answer"
    );
}

impl Space {
    /// @lead asks @reviewer, and the reviewer's prompt hook arms the claim
    /// for its turn. Returns the delivered envelope.
    fn ask_without_prompting(&self) -> Value {
        dispatch::message(
            "send",
            json!({"request_id":"ask","source":"@reviewer Reply with the single word pong.",
                   "author_kind":"agent_session"}),
            &self.runtime("pane-lead"),
        )
        .unwrap();
        self.herdr.envelope_for("pane-reviewer")
    }

    fn armed_ask(&self) -> Value {
        let ask = self.ask_without_prompting();
        self.prompt(
            "pane-reviewer",
            "session-reviewer",
            CLAUDE,
            &ask.to_string(),
        );
        ask
    }

    fn claims_file(&self) -> PathBuf {
        let mut sessions = fs::read_dir(self.directory.state.join("final-reply")).unwrap();
        let file = sessions.next().unwrap().unwrap().path().join("claims.json");
        assert!(sessions.next().is_none());
        file
    }

    /// The body of every envelope that reached `pane`, from this process's
    /// drains and from the binary's.
    fn answers(&self, pane: &str) -> Vec<String> {
        let prompts = self.herdr.prompts.lock().unwrap();
        prompts
            .iter()
            .filter(|(to, _)| to == pane)
            .map(|(_, envelope)| delivered::envelope_of(envelope))
            .chain(self.prompted_by_binary(pane))
            .map(|envelope: Value| envelope["untrusted_body"].as_str().unwrap().to_owned())
            .collect()
    }

    /// What `delivery inbox` prints for `pane`, run as the agent runs it.
    fn inbox(&self, pane: &str) -> Vec<Value> {
        let output = self
            .command()
            .arg("--plugin-state")
            .arg(&self.directory.state)
            .args(["delivery", "inbox", "--herdr-socket", "socket-a"])
            .args(["--herdr-workspace", "workspace-1", "--herdr-pane", pane])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let printed: Value = serde_json::from_slice(&output.stdout).unwrap();
        printed["data"]["envelopes"].as_array().unwrap().clone()
    }

    fn bodies(&self) -> Vec<String> {
        self.rows("SELECT body FROM collaboration_messages ORDER BY created_at_ms, rowid")
            .into_iter()
            .map(|row| row[0].clone())
            .collect()
    }
}

/// A Stop hook the host runs again for the same turn, at once or after the
/// first worker died before it recorded the claim, settles the delivery once.
#[test]
fn a_retried_stop_hook_settles_the_delivery_once() {
    let space = Space::new();
    let ask = space.armed_ask();
    let attempt = ask["control"]["attempt_id"].as_str().unwrap().to_owned();
    let armed = fs::read(space.claims_file()).unwrap();

    let first = space.start_installed_stop("session-reviewer", CLAUDE.1, "pong");
    let retry = space.start_installed_stop("session-reviewer", CLAUDE.1, "pong");
    first.wait();
    retry.wait();
    space.installed_stop("session-reviewer", CLAUDE.1, "pong");
    fs::write(space.claims_file(), &armed).unwrap();
    space.installed_stop("session-reviewer", CLAUDE.1, "pong");

    assert_eq!(space.dispositions(), [vec![attempt, "reply".to_owned()]]);
    assert_eq!(space.bodies(), ["Reply with the single word pong.", "pong"]);
    assert_eq!(space.answers("pane-lead"), ["pong"]);
    assert!(
        space.claims()[0]["state"]["done"].is_string(),
        "{:?}",
        space.claims()
    );
}

/// A Stop worker that dies once its reply is recorded, before any mailbox
/// worker runs, leaves the reply queued: the restart's reconciliation delivers
/// it once, and neither the host's retried Stop nor a later restart sends it
/// again.
#[test]
fn a_reply_recorded_before_the_stop_worker_died_is_delivered_once_after_a_restart() {
    let space = Space::new();
    let ask = space.armed_ask();
    let attempt = ask["control"]["attempt_id"].as_str().unwrap().to_owned();
    let stop = stop_facts("session-reviewer", CLAUDE, "pong", false);
    let died = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        settle_at_stop(
            &space.directory.state,
            "socket-a",
            &stop,
            42,
            &mut |bound| {
                execute_message_operation(
                    bound.operation,
                    expand_attempt_reply(bound.input, Some(&bound.state))?,
                    &space.runtime(&bound.pane),
                )?;
                std::panic::resume_unwind(Box::new("the Stop worker died"))
            },
        );
    }));
    assert!(died.is_err());
    assert_eq!(
        space.dispositions(),
        [vec![attempt.clone(), "reply".to_owned()]]
    );
    assert!(space.answers("pane-lead").is_empty());

    let restart = || {
        execute_lifecycle_operation("reconcile", json!({}), &space.runtime("pane-lead")).unwrap();
    };
    restart();
    assert_eq!(space.answers("pane-lead"), ["pong"]);
    space.stop("session-reviewer", CLAUDE, "pong");
    restart();

    assert_eq!(space.dispositions(), [vec![attempt, "reply".to_owned()]]);
    assert_eq!(space.bodies(), ["Reply with the single word pong.", "pong"]);
    assert_eq!(space.answers("pane-lead"), ["pong"]);
    assert!(
        space.claims()[0]["state"]["done"].is_string(),
        "{:?}",
        space.claims()
    );
}

/// An agent that settles with its bound line before its turn ends has
/// answered: the installed Stop hook that follows records and sends nothing.
#[test]
fn an_explicit_settle_leaves_the_stop_hook_nothing_to_do() {
    for (line, kind, answers) in [
        ("reply", "reply", vec!["pong"]),
        ("complete.no-reply", "no_reply", vec![]),
    ] {
        let space = Space::new();
        let ask = space.armed_ask();
        let attempt = ask["control"]["attempt_id"].as_str().unwrap().to_owned();
        let reviewer = space.runtime("pane-reviewer");
        match line {
            "reply" => {
                dispatch::message("reply", bound_reply(&space, &ask, "@lead pong"), &reviewer)
            }
            _ => dispatch::integration("complete", json!({"attempt_id": attempt}), &reviewer),
        }
        .unwrap();

        space.installed_stop("session-reviewer", CLAUDE.1, "I answered @lead: pong.");

        assert_eq!(
            space.dispositions(),
            [vec![attempt, kind.to_owned()]],
            "{line}"
        );
        let mut bodies = vec!["Reply with the single word pong."];
        bodies.extend(&answers);
        assert_eq!(space.bodies(), bodies, "{line}");
        assert_eq!(space.answers("pane-lead"), answers, "{line}");
        assert_eq!(
            space.claims()[0]["state"],
            json!({"done":"superseded"}),
            "{line}"
        );
    }
}

/// A turn whose Stop hook never runs leaves its delivery pending: `delivery
/// inbox` hands it back to that session in a later turn, and its bound reply
/// settles it once however often the agent runs it.
#[test]
fn a_delivery_whose_stop_never_ran_stays_in_the_inbox_until_settled_once() {
    let space = Space::new();
    let ask = space.armed_ask();
    let attempt = ask["control"]["attempt_id"].as_str().unwrap().to_owned();
    let attempts = |envelopes: &[Value]| -> Vec<String> {
        envelopes
            .iter()
            .map(|envelope| {
                envelope["control"]["attempt_id"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect()
    };

    execute_user_prompt_submit_hook(
        &json!({"hook_event_name":"UserPromptSubmit","session_id":"session-reviewer",
                "prompt_id":"later-turn","prompt":"Anything waiting for you?"})
        .to_string(),
        &space.runtime("pane-reviewer"),
    );
    space.installed_stop("session-reviewer", "later-turn", "Let me check my inbox.");
    assert!(space.dispositions().is_empty());
    assert!(space.answers("pane-lead").is_empty());
    assert_eq!(space.claims()[0]["state"], json!({"left":"interrupted"}));
    assert!(attempts(&space.inbox("pane-lead")).is_empty());
    let recovered = space.inbox("pane-reviewer");
    assert_eq!(attempts(&recovered), [attempt.as_str()]);

    for _ in 0..2 {
        dispatch::message(
            "reply",
            bound_reply(&space, &recovered[0], "@lead pong"),
            &space.runtime("pane-reviewer"),
        )
        .unwrap();
    }
    assert_eq!(space.dispositions(), [vec![attempt, "reply".to_owned()]]);
    assert_eq!(space.bodies(), ["Reply with the single word pong.", "pong"]);
    assert_eq!(space.answers("pane-lead"), ["pong"]);
    assert!(attempts(&space.inbox("pane-reviewer")).is_empty());
}

/// Every way a claim can end without settling leaves the delivery open after
/// the hook said to run no settle command, so every later prompt shows the
/// line that settles it, until it is settled.
#[test]
fn every_unsettled_ending_shows_the_way_back_on_later_prompts() {
    let way_back = |context: &str, attempt: &str, how: &str| {
        context.contains(&format!(
            "Namedrop: delivery {attempt} from an earlier turn is still open; {how}.\n"
        )) && context.contains(&format!(r#""attempt_id":"{attempt}""#))
    };
    const ANSWER: &str = "to answer it, run this line with your text in place of <reply body>";
    for (ending, state) in [
        ("no_final_text", json!({"left":"no_final_text"})),
        ("interrupted", json!({"left":"interrupted"})),
    ] {
        let space = Space::new();
        let ask = space.armed_ask();
        let attempt = ask["control"]["attempt_id"].as_str().unwrap().to_owned();
        if ending == "no_final_text" {
            space.stop("session-reviewer", CLAUDE, "  ");
        }
        let typed = space.prompt(
            "pane-reviewer",
            "session-reviewer",
            ("prompt_id", "typed-turn"),
            "Where were we?",
        );
        assert_eq!(space.claims()[0]["state"], state, "{ending}");
        assert!(way_back(&typed, &attempt, ANSWER), "{ending}: {typed}");
        assert!(space.dispositions().is_empty(), "{ending}");

        dispatch::message(
            "reply",
            bound_reply(&space, &ask, "@lead pong"),
            &space.runtime("pane-reviewer"),
        )
        .unwrap();
        let after = execute_user_prompt_submit_hook(
            &json!({"hook_event_name":"UserPromptSubmit","session_id":"session-reviewer",
                    "prompt_id":"after-turn","prompt":"Thanks."})
            .to_string(),
            &space.runtime("pane-reviewer"),
        );
        let after = after
            .as_ref()
            .and_then(|output| output["hookSpecificOutput"]["additionalContext"].as_str())
            .unwrap_or_default();
        assert!(!after.contains("is still open"), "{ending}: {after}");
    }

    let space = Space::new();
    let ask = space.armed_ask();
    space.stop("session-reviewer", CLAUDE, "pong");
    let answer = space.herdr.envelope_for("pane-lead");
    let attempt = answer["control"]["attempt_id"].as_str().unwrap().to_owned();
    assert_ne!(
        answer["control"]["attempt_id"],
        ask["control"]["attempt_id"]
    );
    space.prompt("pane-lead", "session-lead", CODEX, &answer.to_string());
    let typed = space.prompt(
        "pane-lead",
        "session-lead",
        ("turn_id", "typed-turn"),
        "What did the reviewer say?",
    );
    assert!(
        way_back(&typed, &attempt, "to close it, run this line"),
        "{typed}"
    );
    assert!(typed.contains("complete.no-reply: "), "{typed}");
}

/// A prompt that leads with a Mention Namedrop cannot send still reaches the
/// agent, so it still says how to settle a delivery left open.
#[test]
fn a_leading_mention_prompt_keeps_the_way_back() {
    let space = Space::new();
    let ask = space.armed_ask();
    let attempt = ask["control"]["attempt_id"].as_str().unwrap().to_owned();
    space.stop("session-reviewer", CLAUDE, " ");
    let context = space.prompt(
        "pane-reviewer",
        "session-reviewer",
        ("prompt_id", "mention-turn"),
        "@ghost please take a look",
    );
    assert!(context.contains("Namedrop could not resolve"), "{context}");
    assert!(
        context.contains(&format!(
            "Namedrop: delivery {attempt} from an earlier turn is still open;"
        )),
        "{context}"
    );
}

/// When another Stop hook blocks, Claude Code continues the turn, so the
/// text that Stop carried is not the final message. Namedrop settles at the
/// turn's next Stop, with the text the turn really ended on.
#[test]
fn a_stop_another_hook_blocked_settles_at_the_turns_next_stop() {
    let space = Space::new();
    space.armed_ask();
    // The installed prompt hook records the turn's start.
    space.reviewer_edge(namedrop::turn_phase::TurnEdge::Prompt, now_ms());
    let transcript = space.directory.root.join("transcript.jsonl");
    let append = |records: &[Value]| {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&transcript)
            .unwrap();
        for record in records {
            writeln!(file, "{record}").unwrap();
        }
    };
    let assistant = |text: &str| {
        json!({"type":"assistant","sessionId":"session-reviewer",
               "message":{"role":"assistant","content":[{"type":"text","text":text}]}})
    };
    let recorded: Vec<Value> =
        include_str!("fixtures/final-reply/claude-2.1.319-blocked-stop.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    let (feedback, blocked, summary) = (&recorded[0], &recorded[1], &recorded[2]);
    let marker = || {
        let session = fs::read_dir(space.directory.state.join("final-reply"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        session.join("stops").join(
            HostTurn::from_ids(Some("reviewer-turn"), None)
                .unwrap()
                .as_str(),
        )
    };
    // Claude Code writes how the Stop's hooks ended once they have all run,
    // so after this hook measured the transcript and recorded the Stop.
    let stop = |text: &str, active: bool| {
        let _ = fs::remove_file(marker());
        let running = space.start_installed_stop_with(json!({"hook_event_name":"Stop",
            "session_id":"session-reviewer","prompt_id":"reviewer-turn",
            "transcript_path":transcript,"stop_hook_active":active,
            "last_assistant_message":text,"background_tasks":[]}));
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !marker().exists() {
            assert!(
                std::time::Instant::now() < until,
                "the Stop hook recorded no Stop"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        running
    };

    append(&[assistant("Let me check one thing first.")]);
    let first = stop("Let me check one thing first.", false);
    append(&[feedback.clone(), blocked.clone()]);
    first.wait();
    assert!(
        space.dispositions().is_empty(),
        "{:?}",
        space.dispositions()
    );
    assert_eq!(space.claims()[0]["state"], json!("open"));
    let phase =
        || namedrop::turn_phase::read(&space.directory.state, "socket-a", "session-reviewer");
    // The turn goes on, so delivery must not take it for one between turns.
    assert!(
        matches!(
            phase(),
            Some(namedrop::turn_phase::TurnPhase::Prompted { .. })
        ),
        "{:?}",
        phase()
    );

    append(&[assistant("pong")]);
    let second = stop("pong", true);
    append(std::slice::from_ref(summary));
    second.wait();
    assert!(
        matches!(
            phase(),
            Some(namedrop::turn_phase::TurnPhase::Stopped { .. })
        ),
        "{:?}",
        phase()
    );
    assert_eq!(space.dispositions()[0][1], "reply");
    assert_eq!(space.answers("pane-lead"), ["pong"]);
    assert_eq!(space.claims()[0]["state"], json!({"done":"replied"}));
}

/// A retried Stop may carry other text, as when the host replays a Stop
/// after the turn's text changed. The delivery still gets one answer: the
/// first Stop's.
#[test]
fn a_retried_stop_with_other_text_settles_the_delivery_once() {
    let space = Space::new();
    let ask = space.armed_ask();
    let attempt = ask["control"]["attempt_id"].as_str().unwrap().to_owned();
    let armed = fs::read(space.claims_file()).unwrap();
    space.installed_stop("session-reviewer", CLAUDE.1, "pong");
    space.installed_stop("session-reviewer", CLAUDE.1, "pong, and one more thing.");
    fs::write(space.claims_file(), &armed).unwrap();
    space.installed_stop("session-reviewer", CLAUDE.1, "A different answer.");
    assert_eq!(space.dispositions(), [vec![attempt, "reply".to_owned()]]);
    assert_eq!(space.bodies(), ["Reply with the single word pong.", "pong"]);
    assert_eq!(space.answers("pane-lead"), ["pong"]);

    let space = Space::new();
    space.send_as_person("question", "@reviewer Option A or option B?", None);
    let question = space.herdr.envelope_for("pane-reviewer");
    let attempt = question["control"]["attempt_id"]
        .as_str()
        .unwrap()
        .to_owned();
    space.prompt(
        "pane-reviewer",
        "session-reviewer",
        CLAUDE,
        &question.to_string(),
    );
    let armed = fs::read(space.claims_file()).unwrap();
    space.installed_stop("session-reviewer", CLAUDE.1, "Option B.");
    space.installed_stop("session-reviewer", CLAUDE.1, "Option C.");
    fs::write(space.claims_file(), &armed).unwrap();
    space.installed_stop("session-reviewer", CLAUDE.1, "Option C.");
    assert_eq!(
        space
            .relayed()
            .iter()
            .map(|row| row[3].as_str())
            .collect::<Vec<_>>(),
        ["Option B."]
    );
    assert_eq!(space.dispositions(), [vec![attempt, "no_reply".to_owned()]]);
}

/// A prompt typed into the pane is never a delivery, whatever claim the
/// session holds: its final message is posted nowhere, and an armed claim
/// it interrupts or a held one stays open for the agent to settle.
#[test]
fn a_typed_prompt_after_an_armed_or_held_claim_is_never_sent() {
    for held in [false, true] {
        let space = Space::new();
        space.send_as_person("question", "@reviewer Run the build and report.", None);
        let question = space.herdr.envelope_for("pane-reviewer");
        let attempt = question["control"]["attempt_id"]
            .as_str()
            .unwrap()
            .to_owned();
        space.prompt(
            "pane-reviewer",
            "session-reviewer",
            CLAUDE,
            &question.to_string(),
        );
        if held {
            space.stop_with("session-reviewer", CLAUDE, "The build is running.", true);
        }
        let typed = space.prompt(
            "pane-reviewer",
            "session-reviewer",
            ("prompt_id", "typed-turn"),
            "Which file did you change?",
        );
        if held {
            // A held claim's Stop starts a worker, and the installed one runs
            // on the real clock, where this test's claim is a day old.
            space.stop(
                "session-reviewer",
                ("prompt_id", "typed-turn"),
                "I changed src/main.rs.",
            );
        } else {
            space.installed_stop("session-reviewer", "typed-turn", "I changed src/main.rs.");
        }
        assert!(space.relayed().is_empty(), "{held}: {:?}", space.relayed());
        assert!(space.dispositions().is_empty(), "{held}");
        let state = if held { "held" } else { "interrupted" };
        assert_eq!(space.claims()[0]["state"], json!({"left": state}));
        assert!(
            typed.contains(&format!(
                "Namedrop: delivery {attempt} from an earlier turn"
            )),
            "{held}: {typed}"
        );
    }
}

/// The Stop worker's own pipeline, the one the CLI runs, spends the thread's
/// Automatic Reply Limit: the reply that reaches it pauses the thread and
/// holds the answer.
#[test]
fn the_stop_workers_pipeline_spends_the_automatic_reply_limit() {
    let space = Space::new();
    space.armed_ask();
    let thread = space.rows("SELECT thread_id FROM collaboration_messages WHERE request_id='ask'")
        [0][0]
        .clone();
    rusqlite::Connection::open(space.directory.state.join("namedrop.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE thread_controls SET reply_allowance=1 WHERE thread_id=?1",
            [&thread],
        )
        .unwrap();
    space.installed_stop("session-reviewer", CLAUDE.1, "pong");
    assert_eq!(space.dispositions()[0][1], "reply");
    assert_eq!(space.bodies(), ["Reply with the single word pong.", "pong"]);
    assert_eq!(
        space.rows(&format!(
            "SELECT CAST(paused AS TEXT), CAST(reply_allowance AS TEXT) FROM thread_controls
             WHERE thread_id='{thread}'"
        )),
        [vec!["1".to_owned(), "0".to_owned()]]
    );
    assert_eq!(
        space.rows(&format!(
            "SELECT source_kind FROM attention_items
             WHERE source_id='{thread}' AND resolved_at_ms IS NULL"
        )),
        [vec!["automatic_reply_limit".to_owned()]]
    );
    assert!(
        space.answers("pane-lead").is_empty(),
        "the paused thread delivered the answer"
    );
}

impl Space {
    /// `recipient` holds an armed claim for an ask from `sender`, on the turn
    /// its host names. Returns the delivered envelope.
    fn armed_ask_to(
        &self,
        (sender, recipient, session): (&str, &str, &str),
        turn: (&str, &str),
    ) -> Value {
        dispatch::message(
            "send",
            json!({"request_id":"ask","source":format!("@{recipient} Reply with the single word pong."),
                   "author_kind":"agent_session"}),
            &self.runtime(&format!("pane-{sender}")),
        )
        .unwrap();
        let ask = self.herdr.envelope_for(&format!("pane-{recipient}"));
        self.prompt(
            &format!("pane-{recipient}"),
            session,
            turn,
            &ask.to_string(),
        );
        ask
    }

    fn armed_tracked_ask(&self) -> Value {
        let coordination = namedrop::execute_handoff_operation(
            "handoff.begin",
            json!({"request_id":"begin","deliverable":"A reviewed answer"}),
            &self.runtime("pane-lead"),
        )
        .unwrap()["coordination_id"]
            .clone();
        dispatch::message(
            "send",
            json!({"request_id":"ask","source":"@reviewer Reply with the single word pong.",
                   "author_kind":"agent_session","coordination_id":coordination}),
            &self.runtime("pane-lead"),
        )
        .unwrap();
        let ask = self.herdr.envelope_for("pane-reviewer");
        self.prompt(
            "pane-reviewer",
            "session-reviewer",
            CLAUDE,
            &ask.to_string(),
        );
        ask
    }

    fn last_message_recipients(&self) -> Vec<Vec<String>> {
        self.rows(
            "SELECT alias_snapshot FROM message_recipients WHERE message_id =
             (SELECT message_id FROM collaboration_messages
              ORDER BY created_at_ms DESC, rowid DESC LIMIT 1)",
        )
    }

    fn references(&self) -> Vec<Vec<String>> {
        self.rows("SELECT alias_snapshot FROM context_references")
    }

    fn unresolved_disposition_warnings(&self) -> Vec<Vec<String>> {
        self.rows(
            "SELECT summary FROM attention_items
             WHERE source_kind = 'agent_disposition' AND resolved_at_ms IS NULL",
        )
    }

    fn retired_name(&self, alias: &str) {
        let attribution = MutationAttribution::new(ActorKind::Human, "test", 2);
        let address = PaneAddressId::new(format!("address-{alias}"));
        StateStore::open(&self.directory.state)
            .unwrap()
            .transaction(|transaction| {
                transaction.create_pane_address(
                    &namespace(),
                    &PaneAddressDraft {
                        id: address.clone(),
                        role_id: PaneRoleId::new(format!("role-{alias}")),
                        role_name: alias.to_owned(),
                        role_description: format!("Native collaborator addressed as @{alias}."),
                        alias: alias.to_owned(),
                    },
                    &attribution,
                )?;
                transaction.retire_pane_address(&namespace(), &address, &attribution)
            })
            .unwrap();
    }
}

/// Agents name each other in prose, and the delivery rules say a body Mention
/// adds no recipient. An answer can't depend on who happens to be named in it,
/// on either host.
#[test]
fn a_final_message_naming_an_agent_that_is_not_here_still_answers() {
    for (who, turn) in [
        (("lead", "reviewer", "session-reviewer"), CLAUDE),
        (("reviewer", "lead", "session-lead"), CODEX),
    ] {
        for text in [
            "The result, as relayed from @codex, is pong.",
            "Ask @retired about the rest. The result is pong.",
        ] {
            let space = Space::new();
            space.retired_name("retired");
            let ask = space.armed_ask_to(who, turn);
            let attempt = ask["control"]["attempt_id"].as_str().unwrap().to_owned();
            space.stop(who.2, turn, text);
            assert_eq!(
                space.claims()[0]["state"],
                json!({"done":"replied"}),
                "{text}"
            );
            assert_eq!(space.dispositions(), [vec![attempt, "reply".to_owned()]]);
            assert_eq!(space.bodies().last().unwrap(), text);
            assert_eq!(space.last_message_recipients(), [vec![who.0.to_owned()]]);
            assert!(space.references().is_empty(), "{text}");
            assert!(space.unresolved_disposition_warnings().is_empty(), "{text}");
        }
    }
}

/// A name that does resolve means what it means in any reply: a context
/// reference, never a recipient.
#[test]
fn a_final_message_and_a_reply_treat_a_real_name_the_same_way() {
    let text = "Compare with @lead and @codex. The result is pong.";
    let by_stop = Space::new();
    by_stop.armed_ask();
    by_stop.stop("session-reviewer", CLAUDE, text);
    let by_command = Space::new();
    let ask = by_command.armed_ask();
    dispatch::message(
        "reply",
        bound_reply(&by_command, &ask, &format!("@lead {text}")),
        &by_command.runtime("pane-reviewer"),
    )
    .unwrap();
    for space in [&by_stop, &by_command] {
        assert_eq!(space.bodies().last().unwrap(), text);
        assert_eq!(space.last_message_recipients(), [vec!["lead".to_owned()]]);
        assert_eq!(space.references(), [vec!["lead".to_owned()]]);
    }
}

/// A reply the agent runs itself gets the same answer for the same text.
#[test]
fn an_explicit_reply_naming_an_agent_that_is_not_here_still_answers() {
    for text in [
        "The result, as relayed from @codex, is pong.",
        "Ask @retired about the rest. The result is pong.",
    ] {
        let space = Space::new();
        space.retired_name("retired");
        let ask = space.armed_ask();
        let attempt = ask["control"]["attempt_id"].as_str().unwrap().to_owned();
        dispatch::message(
            "reply",
            bound_reply(&space, &ask, &format!("@lead {text}")),
            &space.runtime("pane-reviewer"),
        )
        .unwrap();
        assert_eq!(space.dispositions(), [vec![attempt, "reply".to_owned()]]);
        assert_eq!(space.bodies().last().unwrap(), text);
        assert_eq!(space.last_message_recipients(), [vec!["lead".to_owned()]]);
        assert!(space.references().is_empty(), "{text}");
    }
}

/// Only an agent's reply loosens: a person replying still names who they mean.
#[test]
fn a_person_replying_still_refuses_an_unknown_body_mention() {
    let space = Space::new();
    let ask = space.armed_ask();
    let refused = dispatch::message(
        "reply",
        json!({"request_id":"person-reply","source":"@lead Compare with @codex.",
               "in_reply_to":ask["control"]["message_id"],"author_kind":"human"}),
        &space.runtime("pane-reviewer"),
    )
    .unwrap_err();
    assert!(refused.contains("unknown Mention: @codex"), "{refused}");
}

/// A new send still names who it reaches, so a typo there is refused.
#[test]
fn a_send_still_refuses_an_unknown_body_mention() {
    let space = Space::new();
    let refused = dispatch::message(
        "send",
        json!({"request_id":"send","source":"@reviewer Compare with @codex.",
               "author_kind":"agent_session"}),
        &space.runtime("pane-lead"),
    )
    .unwrap_err();
    assert!(refused.contains("unknown Mention: @codex"), "{refused}");
}

/// A final message isn't a send, so opening with a name reaches nobody new.
/// The answer still goes to the agent that asked.
#[test]
fn a_final_message_that_opens_with_a_name_still_answers_only_the_asker() {
    for (text, shown) in [
        ("@codex said pong", "\\@codex said pong"),
        ("@reviewer said pong", "\\@reviewer said pong"),
        (
            "@lead @codex, @reviewer: pong",
            "\\@lead \\@codex, \\@reviewer: pong",
        ),
        // Naming the asker can be the point of the sentence, so it stays.
        ("@lead is wrong about pong", "\\@lead is wrong about pong"),
        ("@lead", "\\@lead"),
    ] {
        let space = Space::new();
        space.armed_ask();
        space.stop("session-reviewer", CLAUDE, text);
        assert_eq!(
            space.claims()[0]["state"],
            json!({"done":"replied"}),
            "{text}"
        );
        assert_eq!(space.bodies().last().unwrap(), shown);
        assert_eq!(space.last_message_recipients(), [vec!["lead".to_owned()]]);
        assert!(space.references().is_empty(), "{text}");
    }
}

/// A claim a release before this one left behind still shows its way back.
#[test]
fn a_claim_left_by_an_earlier_release_keeps_its_way_back() {
    let space = Space::new();
    let ask = space.armed_ask();
    let attempt = ask["control"]["attempt_id"].as_str().unwrap().to_owned();
    let mut claims: Value =
        serde_json::from_slice(&fs::read(space.claims_file()).unwrap()).unwrap();
    claims[0]["state"] = json!({"left":"addresses_others"});
    fs::write(space.claims_file(), claims.to_string()).unwrap();
    let typed = space.prompt(
        "pane-reviewer",
        "session-reviewer",
        ("prompt_id", "typed-turn"),
        "Where were we?",
    );
    assert!(
        typed.contains(&format!("delivery {attempt} from an earlier turn")),
        "{typed}"
    );
}

/// When a settle fails for any other reason nothing else would say why: the
/// claim keeps the error, the person gets a warning naming it, the agent's next
/// prompt shows it with the way back, and the agent's answer clears the warning.
#[test]
fn a_settle_that_fails_says_why_and_keeps_the_way_back() {
    for generic_warning_first in [false, true] {
        let space = Space::new();
        let ask = space.armed_ask();
        let attempt = ask["control"]["attempt_id"].as_str().unwrap().to_owned();
        if generic_warning_first {
            StateStore::open(&space.directory.state)
                .unwrap()
                .transaction(|transaction| {
                    transaction.open_attention_item(
                        &namespace(),
                        &namedrop::AttentionItemDraft {
                            id: namedrop::AttentionItemId::new(format!(
                                "attention-agent-disposition-{attempt}"
                            )),
                            source_kind: namedrop::AttentionSourceKind::AgentDisposition,
                            source_id: attempt.clone(),
                            summary: "generic".to_owned(),
                        },
                        &MutationAttribution::new(ActorKind::Plugin, "test", 41),
                    )
                })
                .unwrap();
        }
        let stop = stop_facts("session-reviewer", CLAUDE, "pong", false);
        settle_at_stop(&space.directory.state, "socket-a", &stop, 42, &mut |_| {
            Err("the router is out of cheese".to_owned())
        });
        let claim = &space.claims()[0];
        assert_eq!(claim["state"], json!({"left":"operation_failed"}));
        assert_eq!(claim["failure"], "the router is out of cheese");
        let warnings = space.unresolved_disposition_warnings();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0][0].contains("the router is out of cheese"),
            "{warnings:?}"
        );

        let typed = space.prompt(
            "pane-reviewer",
            "session-reviewer",
            ("prompt_id", "typed-turn"),
            "Where were we?",
        );
        assert!(typed.contains("the router is out of cheese"), "{typed}");
        assert!(
            typed.contains(&format!(r#""attempt_id":"{attempt}""#)),
            "{typed}"
        );

        dispatch::message(
            "reply",
            bound_reply(&space, &ask, "@lead pong"),
            &space.runtime("pane-reviewer"),
        )
        .unwrap();
        assert!(space.unresolved_disposition_warnings().is_empty());
    }
}

/// Tracked work raises no warning of its own while its coordination is open,
/// so a failed settle has to.
#[test]
fn a_tracked_settle_that_fails_warns_the_person_until_the_agent_answers() {
    let space = Space::new();
    let ask = space.armed_tracked_ask();
    assert_eq!(space.claims()[0]["plan"]["reply"]["tracked"], true);
    let stop = stop_facts("session-reviewer", CLAUDE, "pong", false);
    settle_at_stop(&space.directory.state, "socket-a", &stop, 42, &mut |_| {
        Err("the router is out of cheese".to_owned())
    });
    let warnings = space.unresolved_disposition_warnings();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0][0].contains("final message could not be sent")
            && warnings[0][0].contains("the router is out of cheese"),
        "{warnings:?}"
    );
    let mut reply = bound_reply(&space, &ask, "@lead pong");
    reply["work_outcome"] = json!("completed");
    dispatch::message("reply", reply, &space.runtime("pane-reviewer")).unwrap();
    assert!(space.unresolved_disposition_warnings().is_empty());
}

/// When the reviewer's pane is first seen idle past the acknowledgement grace
/// of a delivery dispatched at 42.
const IDLE: u64 = 42 + 60_000;

struct At(u64);
impl Clock for At {
    fn now_unix_ms(&self) -> u64 {
        self.0
    }
    fn wait_ms(&self, _: u64) {}
}

impl Space {
    /// A reviewer whose host runs hooks from its own copy of this package.
    fn reviewer_on(host: &'static str) -> Self {
        let space = Self::new();
        *space.herdr.reviewer_host.lock().unwrap() = host;
        space.install_package("first");
        fs::create_dir_all(space.cached_copy()).unwrap();
        fs::write(space.cached_copy().join("plugin"), "first").unwrap();
        space
    }

    fn cached_copy(&self) -> std::path::PathBuf {
        self.directory.home.join("codex-cache/first")
    }

    fn install_package(&self, executable: &str) {
        let path = self.directory.home.join(executable);
        fs::write(&path, executable).unwrap();
        namedrop::install_integration_package(
            &self.directory.home,
            namedrop::AgentRuntime::Codex,
            &path,
            &self.directory.state,
        )
        .unwrap();
    }

    /// The installed package changes first, and the host's copy follows.
    fn real_plugin_update(&self) {
        self.install_package("second");
        fs::remove_dir_all(self.cached_copy()).unwrap();
    }

    /// A hook as the host starts it, from the copy it holds.
    fn hook_from_cached_copy(&self, session: &str) {
        let mut hook = std::process::Command::new(env!("CARGO_BIN_EXE_namedrop"))
            .args(["hook", "user-prompt-submit", "--state-directory"])
            .arg(&self.directory.state)
            .env_clear()
            .env("HOME", &self.directory.home)
            .env("PLUGIN_ROOT", self.cached_copy())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        use std::io::Write;
        hook.stdin
            .take()
            .unwrap()
            .write_all(
                json!({"hook_event_name":"UserPromptSubmit","session_id":session,"prompt":"hi"})
                    .to_string()
                    .as_bytes(),
            )
            .unwrap();
        hook.wait().unwrap();
    }

    fn typed_prompt(&self, pane: &str, session: &str) {
        execute_user_prompt_submit_hook(
            &json!({"hook_event_name":"UserPromptSubmit","session_id":session,
                    "prompt_id":"later-turn","prompt":"Anything waiting for you?"})
            .to_string(),
            &self.runtime(pane),
        );
    }

    fn observe_settled(&self) {
        self.observe_at(IDLE);
    }

    /// Reconcile at `now_ms`, with every pane idle. The drain report's
    /// earliest due time, when it must run again with no other event.
    /// The lead pane's runtime with `clock`.
    fn runtime_at<'a>(&'a self, clock: &'a At) -> LifecycleRuntime<'a, FakeMux, At, SequenceIds> {
        let runtime = self.runtime("pane-lead");
        LifecycleRuntime {
            clock,
            mux: runtime.mux,
            ids: runtime.ids,
            state_directory: runtime.state_directory,
            home_directory: runtime.home_directory,
            agent_config: runtime.agent_config,
            cwd: runtime.cwd,
            workspace_id: runtime.workspace_id,
            current_pane_id: runtime.current_pane_id,
            session_socket_id: runtime.session_socket_id,
        }
    }

    fn observe_at(&self, now_ms: u64) -> Option<u64> {
        let reconciled = namedrop::execute_lifecycle_operation(
            "reconcile",
            json!({}),
            &self.runtime_at(&At(now_ms)),
        )
        .unwrap();
        serde_json::from_value::<namedrop::DrainReport>(reconciled["drain"].clone())
            .unwrap()
            .next_due_ms()
    }

    fn refused_sessions_in_doctor(&self) -> Vec<String> {
        namedrop::diagnose_collaboration(&self.runtime("pane-lead"))
            .issues
            .into_iter()
            .filter(|issue| issue.code == "ND-SETUP-005")
            .map(|issue| issue.summary)
            .collect()
    }
}

/// Codex refuses a running session's hooks once the plugin changes, so that
/// session's Stop never runs and nothing there can report the lost answer.
#[test]
fn a_session_codex_refuses_warns_at_settle_even_for_tracked_work() {
    let space = Space::reviewer_on("codex");
    space.hook_from_cached_copy("session-reviewer");
    space.armed_tracked_ask();
    space.real_plugin_update();
    space.observe_settled();
    let warnings = space.unresolved_disposition_warnings();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0][0].contains("new Codex session"), "{warnings:?}");
}

#[test]
fn an_untracked_warning_says_codex_refused_the_hooks() {
    let space = Space::reviewer_on("codex");
    space.hook_from_cached_copy("session-reviewer");
    space.armed_ask();
    space.real_plugin_update();
    space.observe_settled();
    let warnings = space.unresolved_disposition_warnings();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0][0].contains("new Codex session"), "{warnings:?}");
}

#[test]
fn a_session_whose_hooks_still_run_raises_nothing_new() {
    let space = Space::reviewer_on("codex");
    space.hook_from_cached_copy("session-reviewer");
    space.armed_tracked_ask();
    space.observe_settled();
    assert!(space.unresolved_disposition_warnings().is_empty());
}

#[test]
fn only_codex_refuses_hooks_when_the_package_changes() {
    let space = Space::reviewer_on("claude");
    space.hook_from_cached_copy("session-reviewer");
    space.armed_tracked_ask();
    space.real_plugin_update();
    space.observe_settled();
    assert!(space.unresolved_disposition_warnings().is_empty());
    assert!(space.refused_sessions_in_doctor().is_empty());
}

/// Before any answer is lost, and until a hook runs there again.
#[test]
fn doctor_names_a_running_session_whose_hooks_are_refused() {
    let space = Space::reviewer_on("codex");
    assert!(
        space.refused_sessions_in_doctor().is_empty(),
        "a session that has run no hook yet may just be starting"
    );
    space.hook_from_cached_copy("session-reviewer");
    space.armed_ask();
    assert!(space.refused_sessions_in_doctor().is_empty());

    space.real_plugin_update();
    let refused = space.refused_sessions_in_doctor();
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert!(refused[0].contains("@reviewer"), "{refused:?}");

    fs::create_dir_all(space.cached_copy()).unwrap();
    space.hook_from_cached_copy("session-reviewer");
    space.typed_prompt("pane-reviewer", "session-reviewer");
    assert!(space.refused_sessions_in_doctor().is_empty());
}

/// Hooks run from the host's copy, which lags the installed package. A prompt in
/// that gap still runs the old copy.
#[test]
fn a_hook_between_the_package_update_and_the_cache_refresh_keeps_its_copy() {
    let space = Space::reviewer_on("codex");
    space.hook_from_cached_copy("session-reviewer");
    space.install_package("second");
    space.hook_from_cached_copy("session-reviewer");
    space.armed_ask();
    assert!(
        space.refused_sessions_in_doctor().is_empty(),
        "the session's copy is still there"
    );

    fs::remove_dir_all(space.cached_copy()).unwrap();
    let refused = space.refused_sessions_in_doctor();
    assert_eq!(refused.len(), 1, "{refused:?}");
}

#[test]
fn a_cache_refresh_that_failed_refuses_nobody() {
    let space = Space::reviewer_on("codex");
    space.hook_from_cached_copy("session-reviewer");
    space.armed_ask();
    space.install_package("second");
    assert!(space.refused_sessions_in_doctor().is_empty());
}

/// An old record is forgotten when any later session records its own, so a
/// session in use has to keep its record fresh.
#[test]
fn a_session_in_use_keeps_its_record_past_the_age_limit() {
    let space = Space::reviewer_on("codex");
    space.hook_from_cached_copy("session-reviewer");
    space.armed_ask();
    let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(40 * 24 * 60 * 60);
    for record in fs::read_dir(space.directory.state.join("hook-runs")).unwrap() {
        fs::File::options()
            .write(true)
            .open(record.unwrap().path())
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
    }

    space.hook_from_cached_copy("session-reviewer");
    space.typed_prompt("pane-reviewer", "session-reviewer");
    space.hook_from_cached_copy("session-another");
    space.typed_prompt("pane-lead", "session-lead");

    space.real_plugin_update();
    let refused = space.refused_sessions_in_doctor();
    assert!(
        refused.iter().any(|summary| summary.contains("@reviewer")),
        "{refused:?}"
    );
}

/// A tracked request raises no warning of its own while its coordination is
/// open, so a Stop worker that never finished has to say so.
#[test]
fn a_tracked_claim_whose_stop_worker_was_lost_warns_the_person() {
    let space = Space::new();
    space.armed_tracked_ask();
    let turn = HostTurn::from_ids(Some(CLAUDE.1), None).unwrap();
    mark_stopped(
        &space.directory.state,
        "socket-a",
        "session-reviewer",
        &turn,
        42,
    );
    let later = stop_facts("session-reviewer", ("prompt_id", "later"), "later", false);
    settle_at_stop(
        &space.directory.state,
        "socket-a",
        &later,
        40_000,
        &mut |_| panic!("nothing is left to run"),
    );
    let claim = &space.claims()[0];
    assert_eq!(claim["state"], json!({"left":"operation_failed"}));
    let failure = claim["failure"].as_str().unwrap();
    let warnings = space.unresolved_disposition_warnings();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0][0].contains(failure), "{warnings:?}");
}

/// A Stop that can't take its session's claims leaves them open for the next
/// prompt, which may never come.
#[test]
fn a_tracked_stop_that_misses_the_claims_lock_warns_the_person() {
    let space = Space::new();
    space.armed_tracked_ask();
    let lock = fs::OpenOptions::new()
        .write(true)
        .open(space.claims_file().with_file_name("lock"))
        .unwrap();
    lock.try_lock().unwrap();
    let stop = stop_facts("session-reviewer", CLAUDE, "pong", false);
    settle_at_stop(&space.directory.state, "socket-a", &stop, 42, &mut |_| {
        panic!("the claims are locked")
    });
    drop(lock);
    assert_eq!(space.claims()[0]["state"], "open");
    let warnings = space.unresolved_disposition_warnings();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0][0].contains("claims"), "{warnings:?}");
}

/// The worker's deadline ends it mid-settle, with no chance to save its claim.
#[test]
fn a_tracked_stop_worker_that_hits_its_deadline_warns_the_person() {
    let space = Space::new();
    space.armed_tracked_ask();
    fs::write(space.directory.root.join("herdr-slow"), "").unwrap();
    let running = space.start_installed_stop("session-reviewer", CLAUDE.1, "pong");
    let until = std::time::Instant::now() + std::time::Duration::from_secs(25);
    while space.unresolved_disposition_warnings().is_empty() && std::time::Instant::now() < until {
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    drop(running);
    assert_eq!(space.claims()[0]["state"], "open");
    let warnings = space.unresolved_disposition_warnings();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0][0].contains("10 seconds"), "{warnings:?}");
}

/// Nobody can answer a held or cancelled attempt and acknowledge refuses it,
/// so a warning on one would never clear.
#[test]
fn a_settle_refused_for_a_closed_attempt_opens_no_warning() {
    for cancelled in [false, true] {
        let space = Space::new();
        let ask = space.armed_ask();
        let attempt =
            namedrop::DeliveryAttemptId::new(ask["control"]["attempt_id"].as_str().unwrap());
        let by = MutationAttribution::new(ActorKind::Human, "test", 41);
        StateStore::open(&space.directory.state)
            .unwrap()
            .transaction(|transaction| {
                transaction.hold_delivery_attempt(&namespace(), &attempt, &by)?;
                if cancelled {
                    transaction.transition_delivery_attempt(
                        &namespace(),
                        &attempt,
                        namedrop::DeliveryState::Cancelled,
                        &by,
                    )?;
                }
                Ok(())
            })
            .unwrap();
        space.stop("session-reviewer", CLAUDE, "pong");
        assert_eq!(
            space.claims()[0]["state"],
            json!({"left":"operation_failed"})
        );
        assert!(space.claims()[0]["failure"].is_string());
        assert!(
            space
                .rows("SELECT summary FROM attention_items WHERE source_kind='agent_disposition'")
                .is_empty(),
            "cancelled {cancelled}"
        );
    }
}

/// A warning the person acknowledged stays closed.
#[test]
fn a_failed_settle_does_not_reopen_an_acknowledged_warning() {
    let space = Space::new();
    let ask = space.armed_ask();
    let attempt = ask["control"]["attempt_id"].as_str().unwrap().to_owned();
    let id = namedrop::DeliveryAttemptId::new(attempt.clone());
    let by = MutationAttribution::new(ActorKind::Human, "test", 41);
    StateStore::open(&space.directory.state)
        .unwrap()
        .transaction(|transaction| {
            transaction.open_attention_item(
                &namespace(),
                &namedrop::AttentionItemDraft {
                    id: namedrop::AttentionItemId::new(format!(
                        "attention-agent-disposition-{attempt}"
                    )),
                    source_kind: namedrop::AttentionSourceKind::AgentDisposition,
                    source_id: attempt.clone(),
                    summary: "generic".to_owned(),
                },
                &by,
            )?;
            transaction.observe_agent_session(
                &namespace(),
                "observation-replaced",
                &PaneAddressId::new("address-reviewer"),
                &AgentSessionId::new("session-replaced"),
                "idle",
                &by,
            )?;
            transaction.acknowledge_disposition_attention(&namespace(), &id, &by)
        })
        .unwrap();
    let stop = stop_facts("session-reviewer", CLAUDE, "pong", false);
    settle_at_stop(&space.directory.state, "socket-a", &stop, 42, &mut |_| {
        Err("the router is out of cheese".to_owned())
    });
    assert!(space.unresolved_disposition_warnings().is_empty());
}

/// The person reads the name, not the escape that keeps it from waking anyone.
#[test]
fn the_persons_notification_shows_a_leading_name_without_its_escape() {
    let space = Space::new();
    let context = execute_user_prompt_submit_hook(
        &json!({"hook_event_name":"UserPromptSubmit","session_id":"session-lead",
                "turn_id":"typed","prompt":"@reviewer Who said pong?"})
        .to_string(),
        &space.runtime("pane-lead"),
    );
    assert!(context.is_some());
    dispatch::wake(&space.runtime("pane-lead"));
    let ask = space.herdr.envelope_for("pane-reviewer");
    space.prompt(
        "pane-reviewer",
        "session-reviewer",
        CLAUDE,
        &ask.to_string(),
    );
    space.stop("session-reviewer", CLAUDE, "@ghost said pong");
    assert_eq!(space.bodies().last().unwrap(), "\\@ghost said pong");
    assert_eq!(
        *space.herdr.notifications.lock().unwrap(),
        ["@ghost said pong"]
    );
}

/// A close the Stop could not make is not a message that failed to send.
#[test]
fn a_no_reply_the_stop_could_not_make_says_so() {
    let space = Space::new();
    space.armed_ask();
    space.stop("session-reviewer", CLAUDE, "pong");
    let answer = space.herdr.envelope_for("pane-lead");
    space.prompt("pane-lead", "session-lead", CODEX, &answer.to_string());
    let stop = stop_facts("session-lead", CODEX, "Thanks, noted.", false);
    settle_at_stop(&space.directory.state, "socket-a", &stop, 42, &mut |_| {
        Err("the router is out of cheese".to_owned())
    });
    let warnings = space.unresolved_disposition_warnings();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0][0].contains("could not close this delivery")
            && warnings[0][0].contains("the router is out of cheese")
            && !warnings[0][0].contains("could not be sent"),
        "{warnings:?}"
    );
    let typed = space.prompt(
        "pane-lead",
        "session-lead",
        ("turn_id", "typed-turn"),
        "What did the reviewer say?",
    );
    assert!(
        typed.contains("could not close a delivery")
            && typed.contains("the router is out of cheese")
            && typed.contains("to close it, run this line")
            && !typed.contains("could not be sent"),
        "{typed}"
    );
}

/// The person's thread takes the final message verbatim, whatever it names.
#[test]
fn a_final_message_to_the_person_naming_an_unknown_agent_is_posted() {
    let space = Space::new();
    space.send_as_person("question", "@reviewer Option A or option B?", None);
    let ask = space.herdr.envelope_for("pane-reviewer");
    space.prompt(
        "pane-reviewer",
        "session-reviewer",
        CLAUDE,
        &ask.to_string(),
    );
    let text = "@codex and @lead both say option B.";
    space.stop("session-reviewer", CLAUDE, text);
    assert_eq!(space.relayed()[0][3], text);
    assert_eq!(space.claims()[0]["state"], json!({"done":"no_replied"}));
}

/// The recap is collaborator text typed into the pane, so a pointer whose
/// recap was changed must not authenticate.
#[test]
fn a_pointer_carrying_a_recap_authenticates_and_a_changed_recap_does_not() {
    let space = Space::new();
    space.send_as_person("ask", "@reviewer pick a word", None);
    let ask = space.herdr.envelope_for("pane-reviewer");
    space.prompt(
        "pane-reviewer",
        "session-reviewer",
        CLAUDE,
        &ask.to_string(),
    );
    space.stop("session-reviewer", CLAUDE, "marmalade");
    space.send_as_person("what", "@lead what did @reviewer say?", None);
    let envelope = space.herdr.envelope_for("pane-lead");
    assert!(
        envelope["control"]["untrusted_channel_recap"]
            .as_str()
            .unwrap()
            .contains("marmalade"),
        "{envelope}"
    );
    let typed = typed_pointer(&space, "pane-lead");
    assert!(typed.contains("marmalade"), "{typed}");

    let hook = |prompt: &str| {
        execute_user_prompt_submit_hook(
            &json!({"hook_event_name":"UserPromptSubmit","prompt":prompt,
                    "session_id":"session-lead","prompt_id":"lead-turn"})
            .to_string(),
            &space.runtime("pane-lead"),
        )
        .and_then(|output| {
            output["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .map(str::to_owned)
        })
        .unwrap_or_default()
    };
    let changed = typed.replace("marmalade", "something else");
    assert!(!hook(&changed).contains("Namedrop commands"));
    let authenticated = hook(&typed);
    assert!(
        authenticated.contains("Namedrop commands"),
        "{authenticated}"
    );
    assert!(!authenticated.contains("marmalade"), "{authenticated}");
}

/// The records of each turn in a recorded Claude Code 2.1.319 session, keyed
/// by the turn's `promptId`. A turn's records run from its first record to
/// the next turn's. `prompt-start` puts `task-first` in the background,
/// `prompt-busy` gets that task's notice queued into it, `prompt-wait` puts
/// `task-second` in the background and `prompt-notice` is that task's notice.
fn recorded_turns() -> Vec<(String, Vec<Value>)> {
    turns_of(include_str!(
        "fixtures/final-reply/claude-2.1.319-background-turns.jsonl"
    ))
}

/// The records of each turn in a recorded Claude Code session, keyed by the
/// turn's `promptId`.
fn turns_of(session: &str) -> Vec<(String, Vec<Value>)> {
    let mut turns: Vec<(String, Vec<Value>)> = vec![(String::new(), Vec::new())];
    for line in session.lines() {
        let record: Value = serde_json::from_str(line).unwrap();
        if let Some(prompt) = record["promptId"].as_str()
            && turns.last().unwrap().0 != prompt
        {
            turns.push((prompt.to_owned(), Vec::new()));
        }
        turns.last_mut().unwrap().1.push(record);
    }
    turns
}

/// One recorded turn's records, as the turn `as_turn`.
fn recorded_turn(name: &str, as_turn: &str) -> Vec<Value> {
    turn_of(recorded_turns(), name, as_turn)
}

fn turn_of(turns: Vec<(String, Vec<Value>)>, name: &str, as_turn: &str) -> Vec<Value> {
    let (_, records) = turns
        .into_iter()
        .find(|(prompt, _)| prompt == name)
        .unwrap();
    records
        .into_iter()
        .map(|mut record| {
            if record["promptId"].is_string() {
                record["promptId"] = json!(as_turn);
            }
            record
        })
        .collect()
}

impl Space {
    /// The reviewer's prompt hook on the clock the installed Stop hook uses,
    /// so a claim it touches keeps a time the Stop compares truly. The prompt
    /// may carry no delivery, and then the hook may add nothing. A delivery is
    /// submitted as the pointer typed for it.
    fn reviewer_prompt(&self, turn: &str, prompt: &str) -> Option<String> {
        let prompt = self.as_typed(prompt);
        let input = json!({"hook_event_name":"UserPromptSubmit","prompt":prompt,
                           "session_id":"session-reviewer","prompt_id":turn,
                           "transcript_path":self.transcript()});
        let output = execute_user_prompt_submit_hook(
            &input.to_string(),
            &self.real_runtime("pane-reviewer"),
        )?;
        output["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .map(str::to_owned)
    }

    /// `runtime`, on the clock the installed Stop hook uses.
    fn real_runtime<'a>(
        &'a self,
        pane: &'a str,
    ) -> LifecycleRuntime<'a, FakeMux, namedrop::SystemClock, SequenceIds> {
        LifecycleRuntime {
            state_directory: &self.directory.state,
            home_directory: Some(&self.directory.home),
            agent_config: namedrop::AgentConfigDirectories::default(),
            cwd: &self.directory.repo,
            workspace_id: None,
            current_pane_id: Some(pane),
            session_socket_id: "socket-a",
            mux: &self.herdr.space,
            clock: &namedrop::SystemClock,
            ids: &self.ids,
        }
    }

    fn transcript(&self) -> PathBuf {
        self.directory.root.join("transcript.jsonl")
    }

    fn append_transcript(&self, records: &[Value]) {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.transcript())
            .unwrap();
        for record in records {
            writeln!(file, "{record}").unwrap();
        }
    }

    /// The reviewer's Stop of `turn` as Claude Code sends it, with `running`
    /// still in the background and the transcript at `transcript`.
    fn reviewer_stop(&self, turn: &str, text: &str, running: &[&str], transcript: &Path) {
        let tasks: Vec<Value> = running
            .iter()
            .map(|id| json!({"id":id,"type":"shell","status":"running","description":"recorded"}))
            .collect();
        self.start_installed_stop_with(json!({"hook_event_name":"Stop",
            "session_id":"session-reviewer","prompt_id":turn,"transcript_path":transcript,
            "stop_hook_active":false,"last_assistant_message":text,"background_tasks":tasks}))
            .wait();
    }

    /// The reviewer's delivery turn that ends while `task-first`, which it
    /// started, still runs.
    fn held_on_first_task(&self) -> Value {
        let ask = self.armed_ask();
        self.append_transcript(&recorded_turn("prompt-start", CLAUDE.1));
        self.reviewer_stop(CLAUDE.1, "STARTED", &["task-first"], &self.transcript());
        assert!(self.dispositions().is_empty(), "{:?}", self.dispositions());
        assert_eq!(self.claims()[0]["state"], json!({"left":"held"}));
        ask
    }
}

/// Claude Code queues a task's notice into a turn the person typed, which is
/// about something else. That turn's final message answers the person, never
/// the held delivery: the notice's prompt shows the delivery's reply line, and
/// a missed notice leaves it for the next prompt to show.
#[test]
fn a_notice_queued_into_a_typed_turn_shows_the_held_deliverys_line_instead() {
    let space = Space::new();
    let first = space.held_on_first_task();
    let attempt = first["control"]["attempt_id"].as_str().unwrap().to_owned();
    let way_back = format!("Namedrop: delivery {attempt} from an earlier turn is still open");
    let busy = recorded_turn("prompt-busy", "busy-turn");
    let typed = busy[0]["message"]["content"].as_str().unwrap();
    let queued = busy
        .iter()
        .find(|record| record["attachment"]["type"] == "queued_command")
        .unwrap()["attachment"]["prompt"]
        .as_str()
        .unwrap();
    space.reviewer_prompt("busy-turn", typed);
    // Claude Code 2.1.319 runs the prompt hook for the queued notice, with the
    // running turn's id.
    let context = space
        .reviewer_prompt("busy-turn", queued)
        .unwrap_or_default();
    assert!(
        context.contains(&way_back),
        "the queued notice hid the way back: {context}"
    );
    space.append_transcript(&busy);
    space.reviewer_stop("busy-turn", "SLEPT", &[], &space.transcript());
    assert!(
        space.answers("pane-lead").is_empty(),
        "the typed turn's final message answered the held delivery: {:?}",
        space.answers("pane-lead")
    );
    assert_eq!(space.claims()[0]["state"], json!({"left":"held"}));
    let next = space
        .reviewer_prompt("next-turn", "Anything else?")
        .unwrap_or_default();
    assert!(next.contains(&way_back), "{next}");
}

/// A turn that answers a delivery of its own never also answers one held on a
/// task whose notice it got, so one final message reaches one thread.
#[test]
fn a_turn_with_its_own_delivery_leaves_the_held_one_held() {
    let space = Space::new();
    let first = space.held_on_first_task();
    // The agent going idle settles the attempt's state without a
    // disposition, as reconciliation does, so the mailbox moves on.
    StateStore::open(&space.directory.state)
        .unwrap()
        .transaction(|transaction| {
            transaction.transition_delivery_attempt(
                &namespace(),
                &namedrop::DeliveryAttemptId::new(first["control"]["attempt_id"].as_str().unwrap()),
                namedrop::DeliveryState::Settled,
                &MutationAttribution::new(ActorKind::Plugin, "test", 42),
            )
        })
        .unwrap();
    dispatch::message(
        "send",
        json!({"request_id":"ask-2","source":"@reviewer Reply with the single word ping.",
               "author_kind":"agent_session"}),
        &space.runtime("pane-lead"),
    )
    .unwrap();
    let second = space.herdr.envelope_for("pane-reviewer");
    assert_ne!(
        second["control"]["attempt_id"], first["control"]["attempt_id"],
        "the second ask did not reach the reviewer"
    );
    space.reviewer_prompt("busy-turn", &second.to_string());
    let busy = recorded_turn("prompt-busy", "busy-turn");
    let queued = busy
        .iter()
        .find(|record| record["attachment"]["type"] == "queued_command")
        .unwrap();
    // Claude Code 2.1.319 also runs the prompt hook for a queued notice, with
    // the running turn's id.
    space.reviewer_prompt(
        "busy-turn",
        queued["attachment"]["prompt"].as_str().unwrap(),
    );
    space.append_transcript(&busy);
    space.reviewer_stop("busy-turn", "ping", &[], &space.transcript());
    assert_eq!(space.answers("pane-lead"), ["ping"]);
    let states: Vec<_> = space
        .claims()
        .iter()
        .map(|claim| claim["state"].clone())
        .collect();
    assert_eq!(states, [json!({"left":"held"}), json!({"done":"replied"})]);
}

/// A recorded Claude Code 2.1.322 session whose subagent stops while a
/// background shell of its own still runs. `prompt-agent` starts the subagent
/// `agent-first`, `prompt-interim` is the notice that it waits on its own
/// work, `prompt-hand-back` is its final report, and `prompt-finished` is its
/// last notice, which Claude Code wrote to the transcript only.
const SUBAGENT_SESSION: &str =
    include_str!("fixtures/final-reply/claude-2.1.322-subagent-hand-back.jsonl");

/// One turn of the subagent session up to its end, without what Claude Code
/// queued after it.
fn subagent_turn(name: &str, as_turn: &str) -> Vec<Value> {
    let mut records = turn_of(turns_of(SUBAGENT_SESSION), name, as_turn);
    if let Some(end) = records
        .iter()
        .position(|record| record["subtype"] == "turn_duration")
    {
        records.truncate(end + 1);
    }
    records
}

/// `records` or a prompt, as subagent `agent`'s instead of `agent-first`'s.
fn as_agent<T: serde::Serialize + serde::de::DeserializeOwned>(value: &T, agent: &str) -> T {
    let text = serde_json::to_string(value)
        .unwrap()
        .replace("agent-first", agent);
    serde_json::from_str(&text).unwrap()
}

/// The prompt Claude Code ran the prompt hook with, as its queue logged it:
/// the first one queued in the subagent session that contains `mark`.
fn subagent_prompt(mark: &str) -> String {
    SUBAGENT_SESSION
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|record| record["operation"] == "enqueue")
        .filter_map(|record| record["content"].as_str().map(str::to_owned))
        .find(|content| content.contains(mark))
        .unwrap()
}

impl Space {
    /// The reviewer's delivery turn that ends while the subagent it started,
    /// `agent-first`, still runs. Then Claude Code's notice that the subagent
    /// stopped with a shell of its own running opens a turn, and that turn
    /// ends with a status line while the shell still runs.
    fn held_through_an_interim_notice(&self) -> String {
        let ask = self.armed_ask();
        let attempt = ask["control"]["attempt_id"].as_str().unwrap();
        self.append_transcript(&subagent_turn("prompt-agent", CLAUDE.1));
        self.reviewer_stop(
            CLAUDE.1,
            "I started the subagent.",
            &["agent-first"],
            &self.transcript(),
        );
        assert_eq!(self.claims()[0]["state"], json!({"left":"held"}));
        let way_back = format!(
            "Namedrop: delivery {attempt} from an earlier turn is still open; it waits on background task agent-first; to answer it"
        );
        let context = self
            .reviewer_prompt(
                "interim-turn",
                &subagent_prompt("is waiting on its own background work"),
            )
            .unwrap_or_default();
        assert!(
            context.contains(&way_back),
            "the interim notice hid the way back, or said the task ended: {context}"
        );
        self.append_transcript(&subagent_turn("prompt-interim", "interim-turn"));
        self.reviewer_stop(
            "interim-turn",
            "The subagent is still waiting for its command.",
            &["shell-own"],
            &self.transcript(),
        );
        assert!(
            self.answers("pane-lead").is_empty(),
            "the status line of the turn an interim notice opened answered the delivery: {:?}",
            self.answers("pane-lead")
        );
        assert!(self.dispositions().is_empty(), "{:?}", self.dispositions());
        assert_eq!(self.claims()[0]["state"], json!({"left":"held"}));
        way_back
    }
}

/// Claude Code tells a session that its subagent stopped while background work
/// of the subagent's own still runs, in a notice it marks as possibly interim.
/// The subagent has not finished, so the delivery held on it stays held. Its
/// final report comes as a hand-back, and that turn's final message answers
/// the delivery, once.
#[test]
fn a_notice_that_a_subagent_only_paused_keeps_the_delivery_held_until_its_hand_back() {
    let space = Space::new();
    space.held_through_an_interim_notice();
    let context = space
        .reviewer_prompt("hand-back-turn", &subagent_prompt("[Subagent hand-back]"))
        .unwrap_or_default();
    assert!(
        !context.contains("from an earlier turn is still open"),
        "the turn that answers the delivery also showed its way back: {context}"
    );
    space.append_transcript(&subagent_turn("prompt-hand-back", "hand-back-turn"));
    space.reviewer_stop(
        "hand-back-turn",
        "The command printed sub-ok.",
        &[],
        &space.transcript(),
    );
    assert_eq!(space.answers("pane-lead"), ["The command printed sub-ok."]);
    assert_eq!(space.claims()[0]["state"], json!({"done":"replied"}));

    // A later notice of the same subagent, in a turn of its own, answers nothing.
    let finished = subagent_turn("prompt-finished", "finished-turn");
    space.reviewer_prompt(
        "finished-turn",
        finished[1]["message"]["content"].as_str().unwrap(),
    );
    space.append_transcript(&finished);
    space.reviewer_stop(
        "finished-turn",
        "Nothing more to add.",
        &[],
        &space.transcript(),
    );
    assert_eq!(space.answers("pane-lead"), ["The command printed sub-ok."]);
}

/// A subagent can pause while its session still writes the final message of
/// the turn that started it. No prompt has carried the notice yet, and Claude
/// Code's Stop no longer lists the subagent, only its shell. The notice is in
/// the transcript's queue log, so the answer is held all the same.
#[test]
fn a_subagent_that_pauses_before_its_turns_stop_holds_the_answer() {
    let space = Space::new();
    space.armed_ask();
    space.append_transcript(&subagent_turn("prompt-agent", CLAUDE.1));
    let queued = SUBAGENT_SESSION
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|record| {
            record["operation"] == "enqueue"
                && record["content"]
                    .as_str()
                    .is_some_and(|content| content.contains("is waiting on its own"))
        })
        .unwrap();
    space.append_transcript(&[queued]);
    space.reviewer_stop(
        CLAUDE.1,
        "I started the subagent and will report when it finishes.",
        &["shell-own"],
        &space.transcript(),
    );
    assert!(
        space.answers("pane-lead").is_empty(),
        "{:?}",
        space.answers("pane-lead")
    );
    let claim = &space.claims()[0];
    assert_eq!(claim["state"], json!({"left":"held"}));
    assert_eq!(claim["awaits"], json!(["agent-first"]));

    space.reviewer_prompt("hand-back-turn", &subagent_prompt("[Subagent hand-back]"));
    space.append_transcript(&subagent_turn("prompt-hand-back", "hand-back-turn"));
    space.reviewer_stop(
        "hand-back-turn",
        "The command printed sub-ok.",
        &[],
        &space.transcript(),
    );
    assert_eq!(space.answers("pane-lead"), ["The command printed sub-ok."]);
}

/// With two subagents, one paused and one that hands back, the hand-back's
/// turn does not answer: the paused subagent has not reported, though Claude
/// Code lists neither as running. The paused one's hand-back answers, once.
#[test]
fn a_paused_subagent_holds_the_answer_while_another_subagent_hands_back() {
    let space = Space::new();
    space.armed_ask();
    let mut started = subagent_turn("prompt-agent", CLAUDE.1);
    let launch = started
        .iter()
        .position(|record| record["toolUseResult"]["agentId"] == "agent-first")
        .unwrap();
    started.insert(launch + 1, as_agent(&started[launch], "agent-second"));
    space.append_transcript(&started);
    space.reviewer_stop(
        CLAUDE.1,
        "I started two subagents.",
        &["agent-first", "agent-second"],
        &space.transcript(),
    );
    assert_eq!(
        space.claims()[0]["awaits"],
        json!(["agent-first", "agent-second"])
    );

    space.reviewer_prompt(
        "interim-turn",
        &subagent_prompt("is waiting on its own background work"),
    );
    space.append_transcript(&subagent_turn("prompt-interim", "interim-turn"));
    space.reviewer_stop(
        "interim-turn",
        "One subagent is waiting on its command.",
        &["agent-second", "shell-own"],
        &space.transcript(),
    );

    let hand_back = subagent_prompt("[Subagent hand-back]");
    space.reviewer_prompt("second-turn", &as_agent(&hand_back, "agent-second"));
    space.append_transcript(&as_agent(
        &subagent_turn("prompt-hand-back", "second-turn"),
        "agent-second",
    ));
    space.reviewer_stop(
        "second-turn",
        "One subagent reported; the other is still waiting.",
        &["shell-own"],
        &space.transcript(),
    );
    assert!(
        space.answers("pane-lead").is_empty(),
        "a status line answered while a paused subagent had not reported: {:?}",
        space.answers("pane-lead")
    );
    let claim = &space.claims()[0];
    assert_eq!(claim["state"], json!({"left":"held"}));
    assert_eq!(claim["awaits"], json!(["agent-first"]));

    space.reviewer_prompt("first-turn", &hand_back);
    space.append_transcript(&subagent_turn("prompt-hand-back", "first-turn"));
    space.reviewer_stop("first-turn", "Both reported.", &[], &space.transcript());
    assert_eq!(space.answers("pane-lead"), ["Both reported."]);
    assert_eq!(space.claims()[0]["state"], json!({"done":"replied"}));
}

/// The interim notice says the same task notifies again. When that notice
/// carries no interim mark and opens a turn, as a task's notice does when its
/// report is not handed back, that turn's final message answers the delivery,
/// once.
#[test]
fn the_same_tasks_notice_without_the_interim_mark_answers_the_held_delivery_once() {
    let space = Space::new();
    let way_back = space.held_through_an_interim_notice();
    // A second interim notice changes nothing.
    let again = space
        .reviewer_prompt(
            "interim-again-turn",
            &subagent_prompt("is waiting on its own background work"),
        )
        .unwrap_or_default();
    assert!(again.contains(&way_back), "{again}");
    space.append_transcript(&subagent_turn("prompt-interim", "interim-again-turn"));
    space.reviewer_stop(
        "interim-again-turn",
        "Still waiting.",
        &["shell-own"],
        &space.transcript(),
    );
    assert!(
        space.answers("pane-lead").is_empty(),
        "{:?}",
        space.answers("pane-lead")
    );

    let finished = subagent_turn("prompt-finished", "finished-turn");
    let notice = finished[1]["message"]["content"].as_str().unwrap();
    assert!(notice.contains("<task-id>agent-first</task-id>") && notice.contains("finished"));
    let context = space
        .reviewer_prompt("finished-turn", notice)
        .unwrap_or_default();
    assert!(
        !context.contains("from an earlier turn is still open"),
        "the turn that answers the delivery also showed its way back: {context}"
    );
    space.append_transcript(&finished);
    space.reviewer_stop(
        "finished-turn",
        "The subagent finished: it printed sub-ok.",
        &[],
        &space.transcript(),
    );
    assert_eq!(
        space.answers("pane-lead"),
        ["The subagent finished: it printed sub-ok."]
    );
    assert_eq!(space.claims()[0]["state"], json!({"done":"replied"}));
    let later = space
        .reviewer_prompt("next-turn", "Anything else?")
        .unwrap_or_default();
    assert!(
        !later.contains("from an earlier turn is still open"),
        "{later}"
    );
    space.reviewer_stop("next-turn", "No.", &[], &space.transcript());
    assert_eq!(space.answers("pane-lead").len(), 1);
}

/// A notice is about the task its opening `task-id` names. One quoted in its
/// result wakes nothing.
#[test]
fn a_task_id_quoted_in_a_notice_wakes_nothing() {
    let space = Space::new();
    space.held_on_first_task();
    let mut notice = recorded_turn("prompt-notice", "notice-turn");
    let text = notice[0]["message"]["content"].as_str().unwrap().replace(
        "<task-id>task-second</task-id>",
        "<task-id>task-other</task-id>",
    );
    let text = text.replace(
        "</task-notification>",
        "<result>It waited on <task-id>task-first</task-id>.</result>\n</task-notification>",
    );
    notice[0]["message"]["content"] = json!(text);
    space.reviewer_prompt("notice-turn", &text);
    space.append_transcript(&notice);
    space.reviewer_stop(
        "notice-turn",
        "Something unrelated.",
        &[],
        &space.transcript(),
    );
    assert!(
        space.answers("pane-lead").is_empty(),
        "{:?}",
        space.answers("pane-lead")
    );
    assert_eq!(space.claims()[0]["state"], json!({"left":"held"}));
}

/// When the Stop can't read the turn's transcript, it can't tell what the
/// turn started, so running work holds the final message as before.
#[test]
fn a_stop_that_cannot_read_its_transcript_holds_on_running_work() {
    let space = Space::new();
    space.armed_ask();
    let missing = space.directory.root.join("no-transcript.jsonl");
    space.reviewer_stop(
        CLAUDE.1,
        "I will report back when it ends.",
        &["task-first"],
        &missing,
    );
    assert!(
        space.dispositions().is_empty(),
        "{:?}",
        space.dispositions()
    );
    assert_eq!(space.claims()[0]["state"], json!({"left":"held"}));
}

/// While answering the person's second question in a thread, the agent
/// answers the first one, which its background work held, with that
/// delivery's line. That answer is to another message, so the second
/// question's final message is still relayed.
#[test]
fn an_explicit_answer_to_another_message_in_the_thread_does_not_stand_in_for_this_one() {
    let space = Space::new();
    space.send_as_person(
        "first",
        "@reviewer When the build ends, what did it print?",
        None,
    );
    let first = space.herdr.envelope_for("pane-reviewer");
    let attempt = first["control"]["attempt_id"].as_str().unwrap().to_owned();
    space.prompt(
        "pane-reviewer",
        "session-reviewer",
        CLAUDE,
        &first.to_string(),
    );
    space.stop_with("session-reviewer", CLAUDE, "The build is running.", true);
    StateStore::open(&space.directory.state)
        .unwrap()
        .transaction(|transaction| {
            transaction.transition_delivery_attempt(
                &namespace(),
                &namedrop::DeliveryAttemptId::new(&attempt),
                namedrop::DeliveryState::Settled,
                &MutationAttribution::new(ActorKind::Plugin, "test", 42),
            )
        })
        .unwrap();
    space.send_as_person(
        "second",
        "@reviewer And which file did you change?",
        first["control"]["message_id"].as_str(),
    );
    let second = space.herdr.envelope_for("pane-reviewer");
    assert_ne!(
        second["control"]["attempt_id"],
        first["control"]["attempt_id"]
    );
    space.prompt(
        "pane-reviewer",
        "session-reviewer",
        ("prompt_id", "second-turn"),
        &second.to_string(),
    );
    dispatch::message(
        "reply",
        bound_reply(&space, &first, "@operator The build printed built-ok."),
        &space.runtime("pane-reviewer"),
    )
    .unwrap();
    space.stop(
        "session-reviewer",
        ("prompt_id", "second-turn"),
        "I changed src/main.rs.",
    );
    let bodies: Vec<_> = space
        .relayed()
        .into_iter()
        .map(|row| row[3].clone())
        .collect();
    assert_eq!(bodies, ["I changed src/main.rs."]);
}

/// A delivery that arms no claim, here because the pane's input held a
/// draft, still makes its turn about that delivery. A notice that reaches it
/// never makes its final message the answer to a held one.
#[test]
fn a_turn_that_carries_an_explicit_delivery_wakes_no_held_one() {
    let space = Space::new();
    let first = space.held_on_first_task();
    StateStore::open(&space.directory.state)
        .unwrap()
        .transaction(|transaction| {
            transaction.transition_delivery_attempt(
                &namespace(),
                &namedrop::DeliveryAttemptId::new(first["control"]["attempt_id"].as_str().unwrap()),
                namedrop::DeliveryState::Settled,
                &MutationAttribution::new(ActorKind::Plugin, "test", 42),
            )
        })
        .unwrap();
    dispatch::message(
        "send",
        json!({"request_id":"ask-2","source":"@reviewer Reply with the single word ping.",
               "author_kind":"agent_session"}),
        &space.runtime("pane-lead"),
    )
    .unwrap();
    let second = space.herdr.envelope_for("pane-reviewer");
    assert_ne!(
        second["control"]["attempt_id"],
        first["control"]["attempt_id"]
    );
    let context = space
        .reviewer_prompt(
            "busy-turn",
            &format!(
                "A draft I had not sent. {}",
                space.as_typed(&second.to_string())
            ),
        )
        .unwrap();
    assert!(
        context.contains("already in this pane's input"),
        "{context}"
    );
    let busy = recorded_turn("prompt-busy", "busy-turn");
    space.append_transcript(&busy);
    space.reviewer_stop("busy-turn", "ping", &[], &space.transcript());
    assert!(
        space.answers("pane-lead").is_empty(),
        "{:?}",
        space.answers("pane-lead")
    );
    assert_eq!(space.claims()[0]["state"], json!({"left":"held"}));
}

/// The prompt that carries a held delivery's notice hides that delivery's way
/// back, so the Stop must answer it even when it can't read the notice itself.
#[test]
fn a_held_delivery_its_notice_prompt_promised_wakes_when_the_stop_cannot_read() {
    let space = Space::new();
    space.held_on_first_task();
    let notice = recorded_turn("prompt-notice", "notice-turn");
    let text = notice[0]["message"]["content"]
        .as_str()
        .unwrap()
        .replace("task-second", "task-first");
    let context = space
        .reviewer_prompt("notice-turn", &text)
        .unwrap_or_default();
    assert!(
        !context.contains("from an earlier turn is still open"),
        "{context}"
    );
    let missing = space.directory.root.join("no-transcript.jsonl");
    space.reviewer_stop("notice-turn", "It printed first-done.", &[], &missing);
    assert_eq!(space.answers("pane-lead"), ["It printed first-done."]);
    assert_eq!(space.claims()[0]["state"], json!({"done":"replied"}));
}

/// While a delivery is held, a Stop of a turn its work's notice never reached
/// hands nothing to a worker.
#[test]
fn a_stop_no_held_delivery_can_wake_on_starts_no_worker() {
    let space = Space::new();
    space.held_on_first_task();
    space.reviewer_prompt("typed-turn", "Which file did you change?");
    space.append_transcript(&recorded_turn("prompt-wait", "typed-turn"));
    // Every worker saves the claims by renaming a new file over them.
    let saved = || {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(space.claims_file()).unwrap().ino()
    };
    let before = saved();
    space.reviewer_stop("typed-turn", "src/main.rs.", &[], &space.transcript());
    assert_eq!(saved(), before, "the Stop handed its turn to a worker");
    assert!(space.answers("pane-lead").is_empty());
    assert_eq!(space.claims()[0]["state"], json!({"left":"held"}));
}

impl Space {
    /// The reviewer's attempt `ask` settled with no disposition, as the agent
    /// going idle does, so its mailbox moves on.
    fn settle_attempt(&self, ask: &Value) {
        StateStore::open(&self.directory.state)
            .unwrap()
            .transaction(|transaction| {
                transaction.transition_delivery_attempt(
                    &namespace(),
                    &namedrop::DeliveryAttemptId::new(
                        ask["control"]["attempt_id"].as_str().unwrap(),
                    ),
                    namedrop::DeliveryState::Settled,
                    &MutationAttribution::new(ActorKind::Plugin, "test", 42),
                )
            })
            .unwrap();
    }

    fn second_ask(&self, first: &Value) -> Value {
        self.settle_attempt(first);
        dispatch::message(
            "send",
            json!({"request_id":"ask-2","source":"@reviewer Reply with the single word ping.",
                   "author_kind":"agent_session"}),
            &self.runtime("pane-lead"),
        )
        .unwrap();
        let second = self.herdr.envelope_for("pane-reviewer");
        assert_ne!(
            second["control"]["attempt_id"],
            first["control"]["attempt_id"]
        );
        second
    }
}

impl Space {
    /// Every ND-DELIVERY-003 warning ever raised, open or closed.
    fn disposition_warnings_ever(&self) -> Vec<Vec<String>> {
        self.rows("SELECT summary FROM attention_items WHERE source_kind = 'agent_disposition'")
    }

    fn observe_reviewer_session(&self, session: &str) {
        StateStore::open(&self.directory.state)
            .unwrap()
            .transaction(|transaction| {
                transaction.observe_agent_session(
                    &namespace(),
                    &format!("observation-reviewer-{session}"),
                    &PaneAddressId::new("address-reviewer"),
                    &AgentSessionId::new(session),
                    if session.is_empty() { "empty" } else { "idle" },
                    &MutationAttribution::new(ActorKind::Plugin, "test", 43),
                )
            })
            .unwrap();
    }

    /// Reconcile no longer finding the reviewer's pane for a moment, as while
    /// Herdr restarts.
    fn close_reviewer_pane_for_a_moment(&self) {
        StateStore::open(&self.directory.state)
            .unwrap()
            .transaction(|transaction| {
                transaction.unbind_pane(
                    &namespace(),
                    &PaneAddressId::new("address-reviewer"),
                    &MutationAttribution::new(ActorKind::Plugin, "test", 43),
                )
            })
            .unwrap();
    }

    /// The person closing the reviewer's pane, as Herdr's pane closed event
    /// handles it.
    fn close_reviewer_pane(&self) {
        StateStore::open(&self.directory.state)
            .unwrap()
            .transaction(|transaction| {
                transaction.cancel_and_remove_pane_address(
                    &namespace(),
                    &PaneAddressId::new("address-reviewer"),
                    &MutationAttribution::new(ActorKind::Plugin, "test", 43),
                )
            })
            .unwrap();
    }

    fn retire_reviewer(&self) {
        StateStore::open(&self.directory.state)
            .unwrap()
            .transaction(|transaction| {
                transaction.retire_pane_address(
                    &namespace(),
                    &PaneAddressId::new("address-reviewer"),
                    &MutationAttribution::new(ActorKind::Human, "test", 43),
                )
            })
            .unwrap();
    }

    /// `delivery acknowledge` as the person runs it.
    fn acknowledge(&self, ask: &Value) -> Result<Value, String> {
        namedrop::execute_control_operation(
            "delivery.acknowledge",
            json!({"attempt_id":ask["control"]["attempt_id"],"confirmed":true}),
            &self.runtime("pane-lead"),
        )
    }

    /// The stage Channels draws on the reviewer's arrow of the latest message
    /// sent to it.
    fn reviewer_leg(&self) -> String {
        let rows = StateStore::open(&self.directory.state)
            .unwrap()
            .dashboard_rows(&namespace())
            .unwrap();
        let snapshot = serde_json::to_value(namedrop::dashboard::snapshot::build(
            &rows,
            Some("namedrop/dashboard"),
            1,
        ))
        .unwrap();
        snapshot["messages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|message| message["legs"].as_array().unwrap())
            .rfind(|leg| leg["to"] == "address-reviewer")
            .unwrap()["stage"]["stage"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// The notice of `task-first` opens a turn whose Stop ends with `text`.
    fn woken_by_first_task(&self, text: &str) {
        let notice = recorded_turn("prompt-notice", "notice-turn");
        let prompt = notice[0]["message"]["content"]
            .as_str()
            .unwrap()
            .replace("task-second", "task-first");
        self.reviewer_prompt("notice-turn", &prompt);
        let missing = self.directory.root.join("no-transcript.jsonl");
        self.reviewer_stop("notice-turn", text, &[], &missing);
    }
}

/// Namedrop holds the answer while the agent's background work runs. The
/// agent going idle meanwhile is no reason to tell the person it didn't
/// answer, whichever of the Stop and the idle settle comes first.
#[test]
fn a_held_answer_raises_no_warning_whichever_comes_first() {
    let space = Space::new();
    let ask = space.held_on_first_task();
    space.settle_attempt(&ask);
    assert!(
        space.disposition_warnings_ever().is_empty(),
        "a settle after the Stop held the answer warned: {:?}",
        space.disposition_warnings_ever()
    );
    assert_eq!(
        space.reviewer_leg(),
        "working",
        "Channels says the held answer ended"
    );

    let space = Space::new();
    let ask = space.armed_ask();
    let attempt = ask["control"]["attempt_id"].as_str().unwrap().to_owned();
    space.settle_attempt(&ask);
    assert!(
        space.disposition_warnings_ever().is_empty(),
        "the settle warned before the Stop decided"
    );
    space.append_transcript(&recorded_turn("prompt-start", CLAUDE.1));
    space.reviewer_stop(CLAUDE.1, "STARTED", &["task-first"], &space.transcript());
    assert_eq!(space.claims()[0]["state"], json!({"left":"held"}));
    assert!(
        space.disposition_warnings_ever().is_empty(),
        "the Stop that held the answer left the settle's warning: {:?}",
        space.disposition_warnings_ever()
    );
    let inbox =
        execute_integration_operation("delivery.inbox", json!({}), &space.runtime("pane-reviewer"))
            .unwrap();
    assert_eq!(
        inbox["envelopes"][0]["control"]["attempt_id"], attempt,
        "withdrawing the warning took the held delivery out of the inbox"
    );
}

/// A held answer whose session is gone can never arrive, so the person is
/// told why, can acknowledge it, and is not told again.
#[test]
fn a_held_answer_whose_session_is_gone_warns_once_until_acknowledged() {
    type Case = (&'static str, fn(&Space), &'static str);
    let cases: [Case; 3] = [
        (
            "replaced",
            |space| space.observe_reviewer_session("session-replaced"),
            "A new session took the agent's name",
        ),
        (
            "retired",
            Space::retire_reviewer,
            "or the agent was removed, before it finished",
        ),
        (
            "ended",
            |space| space.observe_reviewer_session(""),
            "If that session comes back, the agent can still answer.",
        ),
    ];
    for (case, gone, says) in cases {
        let space = Space::new();
        let ask = space.held_on_first_task();
        space.settle_attempt(&ask);
        assert!(
            space.disposition_warnings_ever().is_empty(),
            "{case}: warned while the answer was held: {:?}",
            space.disposition_warnings_ever()
        );
        gone(&space);
        let warnings = space.unresolved_disposition_warnings();
        assert_eq!(warnings.len(), 1, "{case}: {warnings:?}");
        assert!(warnings[0][0].contains(says), "{case}: {warnings:?}");
        assert!(
            warnings[0][0].contains("delivery acknowledge clears this warning"),
            "{case}: {warnings:?}"
        );

        space.acknowledge(&ask).unwrap();
        assert!(space.unresolved_disposition_warnings().is_empty(), "{case}");
        if case != "retired" {
            space.observe_reviewer_session("session-later");
        }
        space.observe_settled();
        assert!(
            space.unresolved_disposition_warnings().is_empty(),
            "{case}: an acknowledged warning came back: {:?}",
            space.unresolved_disposition_warnings()
        );
        assert_eq!(space.disposition_warnings_ever().len(), 1, "{case}");
    }
}

/// Closing the pane ends the session that holds the answer, so the person is
/// told, can acknowledge it, and is not told again.
#[test]
fn a_held_answer_whose_pane_is_closed_warns_once_until_acknowledged() {
    let space = Space::new();
    let ask = space.held_on_first_task();
    space.settle_attempt(&ask);
    assert!(space.disposition_warnings_ever().is_empty());
    space.close_reviewer_pane();
    let warnings = space.unresolved_disposition_warnings();
    assert_eq!(warnings.len(), 1, "closing the pane warned nothing");
    assert!(
        warnings[0][0].contains("The agent's pane was closed"),
        "{warnings:?}"
    );
    space
        .acknowledge(&ask)
        .expect("a closed pane leaves nobody to answer");
    assert!(space.unresolved_disposition_warnings().is_empty());
    space.observe_settled();
    assert!(
        space.unresolved_disposition_warnings().is_empty(),
        "an acknowledged warning came back: {:?}",
        space.unresolved_disposition_warnings()
    );
    assert_eq!(space.disposition_warnings_ever().len(), 1);
}

/// A pane that shows no session for a moment, as while Herdr restarts, and
/// then the same session again, can still answer: the warning goes away and
/// the held answer arrives.
#[test]
fn a_held_answer_whose_session_comes_back_stops_warning_and_still_arrives() {
    let space = Space::new();
    let ask = space.held_on_first_task();
    let attempt = ask["control"]["attempt_id"].as_str().unwrap().to_owned();
    space.settle_attempt(&ask);
    space.observe_reviewer_session("");
    assert_eq!(space.unresolved_disposition_warnings().len(), 1);
    space.observe_reviewer_session("session-reviewer");
    assert!(
        space.unresolved_disposition_warnings().is_empty(),
        "the session came back and is still warned about: {:?}",
        space.unresolved_disposition_warnings()
    );
    space.register(
        "address-reviewer",
        AgentRuntime::ClaudeCode,
        "pane-reviewer",
    );
    let inbox =
        execute_integration_operation("delivery.inbox", json!({}), &space.runtime("pane-reviewer"))
            .unwrap();
    assert_eq!(
        inbox["envelopes"][0]["control"]["attempt_id"], attempt,
        "the held delivery left the session's inbox"
    );
    space.woken_by_first_task("It printed first-done.");
    assert_eq!(space.answers("pane-lead"), ["It printed first-done."]);
    assert!(
        space.unresolved_disposition_warnings().is_empty(),
        "{:?}",
        space.unresolved_disposition_warnings()
    );
}

/// A session that ended and was then replaced on its name can't be resumed,
/// so the warning says it was replaced.
#[test]
fn a_held_answer_whose_ended_session_is_replaced_says_so() {
    let space = Space::new();
    let ask = space.held_on_first_task();
    space.settle_attempt(&ask);
    space.observe_reviewer_session("");
    space.observe_reviewer_session("session-replaced");
    let warnings = space.unresolved_disposition_warnings();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0][0].contains("A new session took the agent's name")
            && !warnings[0][0].contains("session ended"),
        "{warnings:?}"
    );
}

/// Channels shows a held answer as still being worked on only until the
/// person is told it can never arrive.
#[test]
fn a_held_leg_stops_working_once_its_answer_is_lost() {
    let space = Space::new();
    let ask = space.held_on_first_task();
    space.settle_attempt(&ask);
    assert_eq!(space.reviewer_leg(), "working");
    space.observe_reviewer_session("");
    assert_eq!(space.unresolved_disposition_warnings().len(), 1);
    assert_eq!(
        space.reviewer_leg(),
        "completed",
        "Channels says the agent is still working on an answer it warned is lost"
    );
    space.acknowledge(&ask).unwrap();
    assert_eq!(space.reviewer_leg(), "completed");
}

/// A held delivery whose woken turn ends with no final message went
/// unanswered like any other.
#[test]
fn a_held_answer_whose_woken_turn_says_nothing_warns_as_unanswered() {
    let space = Space::new();
    let ask = space.held_on_first_task();
    space.settle_attempt(&ask);
    assert!(space.disposition_warnings_ever().is_empty());
    space.woken_by_first_task("");
    assert_eq!(space.claims()[0]["state"], json!({"left":"no_final_text"}));
    assert_eq!(space.reviewer_leg(), "completed");
    let warnings = space.unresolved_disposition_warnings();
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0][0].contains("ended its turn without a reply"),
        "{warnings:?}"
    );
}

/// The held answer arrives when the work ends, and the person was never told
/// it was missing.
#[test]
fn a_held_answer_that_arrives_later_never_warns() {
    let space = Space::new();
    let ask = space.held_on_first_task();
    space.settle_attempt(&ask);
    space.woken_by_first_task("It printed first-done.");
    assert_eq!(space.answers("pane-lead"), ["It printed first-done."]);
    assert!(
        space.disposition_warnings_ever().is_empty(),
        "{:?}",
        space.disposition_warnings_ever()
    );
}

/// The idle settle can run a moment before the Stop decides. While the Stop
/// has not decided, the agent going idle is no reason to warn: the Stop
/// warns if its turn ends with no answer, and a session that is gone can't
/// answer at all.
#[test]
fn an_answer_its_stop_has_not_decided_raises_no_warning_until_it_must() {
    let space = Space::new();
    let ask = space.armed_ask();
    space.settle_attempt(&ask);
    assert!(
        space.disposition_warnings_ever().is_empty(),
        "the settle warned before the Stop decided: {:?}",
        space.disposition_warnings_ever()
    );
    space.append_transcript(&recorded_turn("prompt-start", CLAUDE.1));
    space.reviewer_stop(CLAUDE.1, "STARTED", &["task-first"], &space.transcript());
    assert_eq!(space.claims()[0]["state"], json!({"left":"held"}));
    assert!(
        space.disposition_warnings_ever().is_empty(),
        "{:?}",
        space.disposition_warnings_ever()
    );

    let space = Space::new();
    let ask = space.armed_ask();
    space.settle_attempt(&ask);
    space.stop("session-reviewer", CLAUDE, "");
    assert_eq!(space.claims()[0]["state"], json!({"left":"no_final_text"}));
    let warnings = space.unresolved_disposition_warnings();
    assert_eq!(warnings.len(), 1, "a turn with no answer warned nothing");
    assert!(
        warnings[0][0].contains("ended its turn without a reply"),
        "{warnings:?}"
    );

    type Gone = fn(&Space);
    let gone: [Gone; 3] = [
        |space| space.observe_reviewer_session("session-replaced"),
        |space| space.observe_reviewer_session(""),
        Space::close_reviewer_pane,
    ];
    for (case, gone) in gone.into_iter().enumerate() {
        let space = Space::new();
        let ask = space.armed_ask();
        space.settle_attempt(&ask);
        gone(&space);
        let warnings = space.unresolved_disposition_warnings();
        assert_eq!(warnings.len(), 1, "case {case}: {warnings:?}");
        assert!(
            warnings[0][0].contains("ended its turn without a reply"),
            "case {case}: {warnings:?}"
        );
    }
}

/// Only the Stop's judgement warns for a turn that ended with no answer, so
/// a store that stays busy past one try must not lose that warning.
#[test]
fn a_stop_still_warns_when_the_store_is_busy_past_its_first_try() {
    let space = Space::new();
    let ask = space.armed_ask();
    space.settle_attempt(&ask);
    let (locked, lock_taken) = std::sync::mpsc::channel();
    let database = space.directory.state.join("namedrop.sqlite3");
    let busy = std::thread::spawn(move || {
        let connection = rusqlite::Connection::open(database).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        locked.send(()).unwrap();
        // Past the store's own 2 second wait for a lock.
        std::thread::sleep(std::time::Duration::from_millis(2_500));
        connection.execute_batch("COMMIT").unwrap();
    });
    lock_taken.recv().unwrap();
    space.stop("session-reviewer", CLAUDE, "");
    busy.join().unwrap();
    assert_eq!(space.claims()[0]["state"], json!({"left":"no_final_text"}));
    let warnings = space.unresolved_disposition_warnings();
    assert_eq!(warnings.len(), 1, "a busy store lost the warning");
    assert!(
        warnings[0][0].contains("ended its turn without a reply"),
        "{warnings:?}"
    );
}

/// How long Namedrop waits, after the pane is seen idle, for a Stop hook
/// that races the idle status.
const STOP_ARRIVAL_GRACE_MS: u64 = 5_000;
/// How long a Stop's worker may take to decide its claim.
const STOPPED_GRACE_MS: u64 = 30_000;

impl Space {
    /// Whether an ND-DELIVERY-003 warning was ever opened, including one
    /// withdrawn since.
    fn disposition_warning_ever_opened(&self) -> bool {
        !self
            .rows(
                "SELECT entity_id FROM transition_facts
                 WHERE entity_kind = 'attention_item' AND transition_kind = 'opened'
                   AND entity_id LIKE 'attention-agent-disposition-%'",
            )
            .is_empty()
    }

    fn rebind_reviewer_pane(&self) {
        self.rebind_reviewer_pane_at(43);
    }

    /// The reviewer's pane bound to its name again at `now_ms`, and its same
    /// session seen there.
    fn rebind_reviewer_pane_at(&self, now_ms: u64) {
        let at = MutationAttribution::new(ActorKind::Plugin, "test", now_ms);
        StateStore::open(&self.directory.state)
            .unwrap()
            .transaction(|transaction| {
                transaction.rebind_pane(
                    &namespace(),
                    &PaneBindingId::new("binding-reviewer-again"),
                    &PaneAddressId::new("address-reviewer"),
                    "pane-reviewer",
                    &at,
                )?;
                transaction.observe_agent_session(
                    &namespace(),
                    &format!("observation-reviewer-again-{now_ms}"),
                    &PaneAddressId::new("address-reviewer"),
                    &AgentSessionId::new("session-reviewer"),
                    "idle",
                    &at,
                )
            })
            .unwrap();
    }

    /// Herdr's event that the reviewer's pane closed, handled at `now_ms`.
    fn reviewer_pane_closed_at(&self, now_ms: u64) {
        self.reviewer_pane_gone(true);
        namedrop::execute_lifecycle_operation(
            "event.pane-lifecycle",
            json!({"type":"pane_closed","pane_id":"pane-reviewer","workspace_id":"workspace-1"}),
            &self.runtime_at(&At(now_ms)),
        )
        .unwrap();
    }

    fn activate_workspace_2(&self) {
        let workspace_2 = StateNamespace::for_space("socket-a", "workspace-2");
        let mut store = StateStore::open(&self.directory.state).unwrap();
        store
            .transaction(|transaction| {
                transaction.activate_worktree(
                    &workspace_2,
                    &MutationAttribution::new(ActorKind::Human, "test", 1),
                )
            })
            .unwrap();
        store
            .transaction(|transaction| transaction.complete_reconciliation(&workspace_2))
            .unwrap();
    }

    /// Herdr's event that the reviewer's pane moved to workspace-2 as
    /// `pane-elsewhere`, handled at `now_ms`.
    fn reviewer_pane_moved_at(&self, now_ms: u64) {
        self.reviewer_pane_gone(true);
        self.herdr.reviewer_elsewhere.store(true, Ordering::Relaxed);
        namedrop::execute_lifecycle_operation(
            "event.pane-lifecycle",
            json!({"type":"pane_moved","previous_pane_id":"pane-reviewer",
                   "previous_workspace_id":"workspace-1","previous_tab_id":"tab-2",
                   "pane":{"pane_id":"pane-elsewhere","workspace_id":"workspace-2",
                           "terminal_id":"t2","tab_id":"tab-9","focused":false,
                           "agent_status":"working","revision":1}}),
            &self.runtime_at(&At(now_ms)),
        )
        .unwrap();
    }

    /// Herdr's event that `pane-elsewhere` closed, handled at `now_ms`.
    fn moved_pane_closed_at(&self, now_ms: u64) {
        self.herdr
            .reviewer_elsewhere
            .store(false, Ordering::Relaxed);
        namedrop::execute_lifecycle_operation(
            "event.pane-lifecycle",
            json!({"type":"pane_closed","pane_id":"pane-elsewhere","workspace_id":"workspace-2"}),
            &self.runtime_at(&At(now_ms)),
        )
        .unwrap();
    }

    /// Reconcile of workspace-2 at `now_ms`, as `observe_at` is of
    /// workspace-1.
    fn observe_workspace_2_at(&self, now_ms: u64) -> Option<u64> {
        let clock = At(now_ms);
        let runtime = LifecycleRuntime {
            workspace_id: Some("workspace-2"),
            ..self.runtime_at(&clock)
        };
        let reconciled =
            namedrop::execute_lifecycle_operation("reconcile", json!({}), &runtime).unwrap();
        serde_json::from_value::<namedrop::DrainReport>(reconciled["drain"].clone())
            .unwrap()
            .next_due_ms()
    }

    /// Each pane workspace-2's reconcile discovers, with its session.
    fn workspace_2_panes(&self) -> Vec<(String, Option<String>)> {
        namedrop::discover_worktree_for_workspace(&*self.herdr.space, "workspace-2")
            .unwrap()
            .panes
            .iter()
            .map(|pane| {
                (
                    pane.pane_id.clone(),
                    pane.agent_session_id()
                        .map(|session| session.as_str().to_owned()),
                )
            })
            .collect()
    }

    /// Whether Herdr lists the reviewer's pane.
    fn reviewer_pane_gone(&self, gone: bool) {
        self.herdr.reviewer_gone.store(gone, Ordering::Relaxed);
    }

    fn reviewer_bound(&self) -> bool {
        !self
            .rows(
                "SELECT binding_id FROM pane_bindings
                 WHERE address_id = 'address-reviewer' AND active = 1",
            )
            .is_empty()
    }

    /// `delivery acknowledge` as the person runs it at `now_ms`.
    fn acknowledge_at(&self, ask: &Value, now_ms: u64) -> Result<Value, String> {
        namedrop::execute_control_operation(
            "delivery.acknowledge",
            json!({"attempt_id":ask["control"]["attempt_id"],"confirmed":true}),
            &self.runtime_at(&At(now_ms)),
        )
    }

    fn lost_answer_card(&self) -> Value {
        let attention = self.snapshot()["attention"].clone();
        attention
            .as_array()
            .unwrap()
            .iter()
            .find(|card| card["source_kind"] == "agent_disposition")
            .unwrap_or_else(|| panic!("{attention}"))
            .clone()
    }
}

/// How long an address may have no pane before its session counts as ended.
const UNBOUND_GRACE_MS: u64 = 10 * 60 * 1_000;

/// A name the person unbound on purpose outlives the close of the pane it
/// had: the close finds no name on that pane, so it retires nothing,
/// releases no alias and cancels none of the name's mail.
#[test]
fn closing_the_pane_of_a_name_unbound_on_purpose_keeps_the_name_and_its_mail() {
    let space = Space::new();
    let ask = space.held_on_first_task();
    space.settle_attempt(&ask);
    namedrop::execute_lifecycle_operation(
        "address.unbind",
        json!({"address_id":"address-reviewer"}),
        &space.runtime_at(&At(IDLE)),
    )
    .unwrap();
    dispatch::message(
        "send",
        json!({"request_id":"queued","source":"@reviewer One more thing.",
               "author_kind":"agent_session"}),
        &space.runtime("pane-lead"),
    )
    .unwrap();
    let mail = || {
        space.rows(
            "SELECT delivery_id, state FROM mailbox_deliveries
             WHERE address_id = 'address-reviewer' ORDER BY delivery_id",
        )
    };
    let before = mail();
    assert!(
        before.iter().any(|row| row[1] == "queued"),
        "the unbound name queued nothing: {before:?}"
    );

    space.reviewer_pane_closed_at(IDLE + 1);
    assert_eq!(
        space.rows(
            "SELECT CAST(retired AS TEXT) FROM pane_addresses WHERE address_id = 'address-reviewer'"
        ),
        [["0"]],
        "the close retired a name it had no pane of"
    );
    assert_eq!(
        space.rows(
            "SELECT alias FROM alias_claims
             WHERE address_id = 'address-reviewer' AND claim_state = 'active'"
        ),
        [["reviewer"]]
    );
    assert_eq!(mail(), before, "the close changed the name's mail");
    assert_eq!(space.claims()[0]["state"], json!({"left":"held"}));
    assert!(!space.disposition_warning_ever_opened());
}

/// A pane moved to another workspace takes its session along, which can
/// still answer, so no grace runs out on it however long it stays there,
/// whichever workspace drains. Once that pane closes there, the grace runs
/// from the close and the held answer is flagged at its end.
#[test]
fn a_pane_moved_to_another_workspace_loses_its_held_answer_only_once_it_closes() {
    let space = Space::new();
    space.activate_workspace_2();
    let ask = space.held_on_first_task();
    space.settle_attempt(&ask);
    space.reviewer_pane_moved_at(IDLE);
    assert!(!space.reviewer_bound());
    assert_eq!(
        space.workspace_2_panes(),
        [(
            "pane-elsewhere".to_owned(),
            Some("session-reviewer".to_owned())
        )],
        "fixture precondition: the fixture lists the moved pane in workspace-2 until it closes"
    );
    let long_after = IDLE + 3 * UNBOUND_GRACE_MS;
    assert_eq!(space.observe_at(IDLE + UNBOUND_GRACE_MS), None);
    assert_eq!(space.observe_workspace_2_at(IDLE + UNBOUND_GRACE_MS), None);
    assert_eq!(space.observe_at(long_after), None);
    assert_eq!(space.observe_workspace_2_at(long_after), None);
    assert!(
        !space.disposition_warning_ever_opened(),
        "{:?}",
        space.disposition_warnings_ever()
    );
    assert_eq!(space.reviewer_leg(), "working");

    let closed = long_after + 1;
    space.moved_pane_closed_at(closed);
    assert_eq!(
        space.workspace_2_panes(),
        [],
        "fixture precondition: the fixture stops listing the moved pane once it closes"
    );
    let lost_at = closed + UNBOUND_GRACE_MS;
    assert_eq!(space.observe_at(closed), Some(lost_at));
    assert!(!space.disposition_warning_ever_opened());
    assert_eq!(space.observe_at(lost_at), None);
    let warnings = space.unresolved_disposition_warnings();
    assert_eq!(
        warnings.len(),
        1,
        "a moved pane gone for good left it working"
    );
    assert!(
        warnings[0][0].contains("pane has been gone"),
        "{warnings:?}"
    );
    assert_eq!(space.reviewer_leg(), "completed");
}

/// Past `UNBOUND_GRACE_MS` and the few minutes after it that cover a late
/// wake, a drain no longer looks at an address that lost its pane, so its
/// work stays the same however old the database grows. Starting Channels
/// judges what it missed.
#[test]
fn a_drain_long_after_a_pane_went_does_not_look_at_its_attempts() {
    let space = Space::new();
    let ask = space.held_on_first_task();
    space.settle_attempt(&ask);
    space.reviewer_pane_gone(true);
    assert_eq!(space.observe_at(IDLE), Some(IDLE + UNBOUND_GRACE_MS));
    let day = 24 * 60 * 60 * 1_000;
    assert_eq!(space.observe_at(IDLE + day), None);
    assert!(
        !space.disposition_warning_ever_opened(),
        "a drain a day later still judged the pane: {:?}",
        space.disposition_warnings_ever()
    );
    assert_eq!(space.reviewer_leg(), "working");
}

/// A pane that goes away with no close event, as when Herdr restarts and the
/// agent is never resumed, leaves its held answer working only for the
/// grace. Then the next drain, which the grace itself schedules, flags it,
/// and the person can dismiss it.
#[test]
fn a_pane_that_never_comes_back_loses_its_held_answer_after_the_grace() {
    let space = Space::new();
    let ask = space.held_on_first_task();
    space.settle_attempt(&ask);
    space.reviewer_pane_gone(true);
    let lost_at = IDLE + UNBOUND_GRACE_MS;
    assert_eq!(
        space.observe_at(IDLE),
        Some(lost_at),
        "nothing wakes the drain to judge the held answer again"
    );
    assert!(!space.reviewer_bound());
    assert_eq!(space.observe_at(lost_at - 1), Some(lost_at));
    assert!(!space.disposition_warning_ever_opened());
    assert!(space.herdr.notifications.lock().unwrap().is_empty());
    assert_eq!(space.reviewer_leg(), "working");
    assert!(
        space.acknowledge_at(&ask, lost_at - 1).is_err(),
        "a warning nobody raised was acknowledged"
    );

    assert_eq!(space.observe_at(lost_at), None);
    let warnings = space.unresolved_disposition_warnings();
    assert_eq!(warnings.len(), 1, "a pane gone for good left it working");
    assert!(
        warnings[0][0].contains("pane has been gone"),
        "{warnings:?}"
    );
    assert_eq!(space.lost_answer_card()["answer_lost"], "ended");
    assert_eq!(space.reviewer_leg(), "completed");
    space.acknowledge_at(&ask, lost_at).unwrap();
    assert!(space.unresolved_disposition_warnings().is_empty());
}

/// The warning a pane gone past the grace raised goes once the same session
/// is bound and seen again, and its answer is held again.
#[test]
fn a_held_answer_lost_to_the_grace_comes_back_with_its_session() {
    let space = Space::new();
    let ask = space.held_on_first_task();
    space.settle_attempt(&ask);
    space.reviewer_pane_gone(true);
    space.observe_at(IDLE);
    space.observe_at(IDLE + UNBOUND_GRACE_MS);
    assert_eq!(space.unresolved_disposition_warnings().len(), 1);
    space.reviewer_pane_gone(false);
    space.rebind_reviewer_pane_at(IDLE + UNBOUND_GRACE_MS + 1);
    assert!(
        space.unresolved_disposition_warnings().is_empty(),
        "{:?}",
        space.unresolved_disposition_warnings()
    );
    assert_eq!(space.reviewer_leg(), "working");
}

/// A pane reconcile unbinds and that is bound again inside the grace, with
/// its same session, never raises anything, even once the grace is over.
#[test]
fn a_pane_bound_again_inside_the_grace_never_warns() {
    let space = Space::new();
    let ask = space.held_on_first_task();
    space.settle_attempt(&ask);
    space.reviewer_pane_gone(true);
    space.observe_at(IDLE);
    assert!(!space.reviewer_bound());
    space.reviewer_pane_gone(false);
    space.rebind_reviewer_pane_at(IDLE + UNBOUND_GRACE_MS - 1);
    assert_eq!(space.observe_at(IDLE + UNBOUND_GRACE_MS), None);
    assert!(
        !space.disposition_warning_ever_opened(),
        "{:?}",
        space.disposition_warnings_ever()
    );
    assert!(space.herdr.notifications.lock().unwrap().is_empty());
    assert_eq!(space.reviewer_leg(), "working");
}

/// A turn interrupted with Esc, or ended by an API error, fires no Stop. Once
/// its pane has sat idle past the Stop's grace, the person is told the
/// request went unanswered, with no later prompt needed.
#[test]
fn a_turn_that_ends_with_no_stop_warns_while_its_pane_sits_idle() {
    let space = Space::new();
    space.armed_ask();
    let due = space.observe_at(IDLE);
    assert!(
        space.disposition_warnings_ever().is_empty(),
        "warned before a racing Stop could arrive"
    );
    assert_eq!(
        due,
        Some(IDLE + STOP_ARRIVAL_GRACE_MS),
        "nothing wakes the drain to judge the turn again"
    );
    assert_eq!(space.observe_at(IDLE + STOP_ARRIVAL_GRACE_MS - 1), due);
    assert!(space.disposition_warnings_ever().is_empty());
    assert_eq!(space.observe_at(IDLE + STOP_ARRIVAL_GRACE_MS), None);
    let warnings = space.unresolved_disposition_warnings();
    assert_eq!(warnings.len(), 1, "an unanswered request warned nothing");
    assert!(
        warnings[0][0].contains("ended its turn without a reply"),
        "{warnings:?}"
    );
    assert_eq!(space.reviewer_leg(), "completed");
}

/// A Stop that arrives inside the grace and holds the answer leaves the
/// person nothing to act on.
#[test]
fn a_stop_inside_the_grace_that_holds_the_answer_warns_nothing() {
    let space = Space::new();
    space.armed_ask();
    space.observe_at(IDLE);
    space.append_transcript(&recorded_turn("prompt-start", CLAUDE.1));
    space.reviewer_stop(CLAUDE.1, "STARTED", &["task-first"], &space.transcript());
    assert_eq!(space.claims()[0]["state"], json!({"left":"held"}));
    assert_eq!(space.observe_at(IDLE + STOP_ARRIVAL_GRACE_MS), None);
    assert_eq!(space.observe_at(IDLE + STOPPED_GRACE_MS), None);
    assert!(
        !space.disposition_warning_ever_opened(),
        "{:?}",
        space.disposition_warnings_ever()
    );
    assert_eq!(space.reviewer_leg(), "working");
}

/// A Stop that ran but whose worker never decided is given the worker's
/// grace, then judged like a turn with no Stop.
#[test]
fn a_stop_whose_worker_never_finishes_warns_after_the_workers_grace() {
    let space = Space::new();
    space.armed_ask();
    mark_stopped(
        &space.directory.state,
        "socket-a",
        "session-reviewer",
        &HostTurn::from_ids(Some(CLAUDE.1), None).unwrap(),
        IDLE,
    );
    assert_eq!(space.observe_at(IDLE), Some(IDLE + STOPPED_GRACE_MS));
    space.observe_at(IDLE + STOP_ARRIVAL_GRACE_MS);
    assert!(
        space.disposition_warnings_ever().is_empty(),
        "warned while the Stop's worker still had the claim"
    );
    assert_eq!(space.observe_at(IDLE + STOPPED_GRACE_MS), None);
    let warnings = space.unresolved_disposition_warnings();
    assert_eq!(warnings.len(), 1, "a lost worker warned nothing");
    assert!(
        warnings[0][0].contains("ended its turn without a reply"),
        "{warnings:?}"
    );
}

/// A warning that an exited agent can't answer stays while its pane is only
/// unbound, since nothing says the session came back, and goes once the
/// same session is seen again.
#[test]
fn an_unbound_pane_neither_raises_nor_withdraws_a_lost_answer() {
    let space = Space::new();
    let ask = space.held_on_first_task();
    space.settle_attempt(&ask);
    space.observe_reviewer_session("");
    assert_eq!(space.unresolved_disposition_warnings().len(), 1);
    space.close_reviewer_pane_for_a_moment();
    assert_eq!(
        space.unresolved_disposition_warnings().len(),
        1,
        "unbinding the pane withdrew the warning"
    );
    space.rebind_reviewer_pane();
    assert!(
        space.unresolved_disposition_warnings().is_empty(),
        "{:?}",
        space.unresolved_disposition_warnings()
    );
}

/// A Stop that comes after the grace still decides: holding the answer
/// withdraws the warning, and answering closes it.
#[test]
fn a_late_stop_clears_the_warning_the_grace_raised() {
    let space = Space::new();
    let ask = space.armed_ask();
    let attempt = ask["control"]["attempt_id"].as_str().unwrap().to_owned();
    space.observe_at(IDLE);
    space.observe_at(IDLE + STOP_ARRIVAL_GRACE_MS);
    assert_eq!(space.unresolved_disposition_warnings().len(), 1);
    space.append_transcript(&recorded_turn("prompt-start", CLAUDE.1));
    space.reviewer_stop(CLAUDE.1, "STARTED", &["task-first"], &space.transcript());
    assert_eq!(space.claims()[0]["state"], json!({"left":"held"}));
    assert!(
        space.unresolved_disposition_warnings().is_empty(),
        "the held answer is still warned about: {:?}",
        space.unresolved_disposition_warnings()
    );
    let inbox =
        execute_integration_operation("delivery.inbox", json!({}), &space.runtime("pane-reviewer"))
            .unwrap();
    assert_eq!(inbox["envelopes"][0]["control"]["attempt_id"], attempt);

    let space = Space::new();
    space.armed_ask();
    space.observe_at(IDLE);
    space.observe_at(IDLE + STOP_ARRIVAL_GRACE_MS);
    assert_eq!(space.unresolved_disposition_warnings().len(), 1);
    space.stop("session-reviewer", CLAUDE, "It is done.");
    assert_eq!(space.answers("pane-lead"), ["It is done."]);
    assert!(
        space.unresolved_disposition_warnings().is_empty(),
        "the answered request is still warned about: {:?}",
        space.unresolved_disposition_warnings()
    );
}

/// Reconcile may unbind a live pane and bind it again a moment later. The
/// same session is still there, so nothing is ever raised for its held
/// answer or for one its Stop has not decided.
#[test]
fn unbinding_and_rebinding_the_same_session_never_warns() {
    for held in [true, false] {
        let space = Space::new();
        let ask = if held {
            space.held_on_first_task()
        } else {
            space.armed_ask()
        };
        space.settle_attempt(&ask);
        space.close_reviewer_pane_for_a_moment();
        space.rebind_reviewer_pane();
        if held {
            space.observe_at(IDLE + STOPPED_GRACE_MS);
        }
        assert!(
            !space.disposition_warning_ever_opened(),
            "held {held}: {:?}",
            space.disposition_warnings_ever()
        );
        assert_eq!(
            space.reviewer_leg(),
            if held { "working" } else { "completed" },
            "held {held}"
        );
    }
}

impl Space {
    /// Every progress note, as (id, thread, in reply to, body).
    fn notes(&self) -> Vec<Vec<String>> {
        self.rows(
            "SELECT message_id, thread_id, in_reply_to, body FROM collaboration_messages
             WHERE message_id LIKE 'progress-note:%' ORDER BY created_at_ms, rowid",
        )
    }

    fn snapshot(&self) -> Value {
        let rows = StateStore::open(&self.directory.state)
            .unwrap()
            .dashboard_rows(&namespace())
            .unwrap();
        serde_json::to_value(namedrop::dashboard::snapshot::build(
            &rows,
            Some("namedrop/dashboard"),
            1,
        ))
        .unwrap()
    }

    fn wire_message(&self, id: &str) -> Value {
        self.snapshot()["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|message| message["message_id"] == id)
            .unwrap_or_else(|| panic!("{id} is not on the wire"))
            .clone()
    }

    /// The reviewer's Stop of `turn`, in-process, whose prompt was the
    /// notice of `notified`, with `running` started by it and still running.
    fn notice_stop(&self, turn: &str, notified: &str, running: &[&str], text: &str) {
        let running: Vec<String> = running.iter().map(|task| (*task).to_owned()).collect();
        let stop = StopFacts {
            session: "session-reviewer".to_owned(),
            turn: HostTurn::from_ids(Some(turn), None),
            running_tasks: running.clone(),
            started_tasks: running,
            notified_tasks: vec![notified.to_owned()],
            paused_tasks: Vec::new(),
            ended_tasks: Vec::new(),
            read_turn: true,
            text: FinalText::bounded(text),
        };
        settle_at_stop(&self.directory.state, "socket-a", &stop, 42, &mut |bound| {
            self.run(bound)
        });
    }
}

/// While its answer to the person's message is held, the person sees the
/// agent's final message at once, in the thread, marked as a note that it
/// is still working. The note leaves the message open and wakes nobody, and
/// the real answer still comes and closes it.
#[test]
fn a_held_answer_to_the_person_shows_the_agents_words_at_once_as_a_note() {
    let space = Space::new();
    let question = space.send_as_person("question", "@reviewer Run the build and report.", None);
    let thread = question["thread_id"].as_str().unwrap().to_owned();
    let question_id = question["message_id"].as_str().unwrap().to_owned();
    let ask = space.herdr.envelope_for("pane-reviewer");
    let attempt = ask["control"]["attempt_id"].as_str().unwrap().to_owned();
    space.prompt(
        "pane-reviewer",
        "session-reviewer",
        CLAUDE,
        &ask.to_string(),
    );
    space.stop_with("session-reviewer", CLAUDE, "The build is running.", true);
    space.stop_with("session-reviewer", CLAUDE, "The build is running.", true);

    let notes = space.notes();
    assert_eq!(
        notes.len(),
        1,
        "a retried Stop posted the note again: {notes:?}"
    );
    assert!(
        notes[0][0].starts_with(&format!("progress-note:{attempt}:")),
        "{notes:?}"
    );
    assert_eq!(
        notes[0][1..],
        [
            thread.clone(),
            question_id.clone(),
            "The build is running.".to_owned()
        ]
    );
    let note = space.wire_message(&notes[0][0]);
    assert_eq!(note["progress_note"], true, "{note}");
    assert_eq!(note["kind"], "reply");
    assert_eq!(note["author"], "address-reviewer");
    assert_eq!(note["legs"], json!([]));
    assert!(
        space
            .wire_message(&question_id)
            .get("progress_note")
            .is_none()
    );

    assert!(
        space.dispositions().is_empty(),
        "the note closed the message"
    );
    assert_eq!(space.claims()[0]["state"], json!({"left":"held"}));
    let inbox =
        execute_integration_operation("delivery.inbox", json!({}), &space.runtime("pane-reviewer"))
            .unwrap();
    assert_eq!(inbox["envelopes"][0]["control"]["attempt_id"], attempt);
    assert!(space.unresolved_disposition_warnings().is_empty());
    assert_eq!(space.snapshot()["attention"], json!([]));
    assert_eq!(
        space.rows(
            "SELECT CAST(COUNT(*) AS TEXT) FROM message_recipients
             WHERE message_id LIKE 'progress-note:%'"
        ),
        [["0"]],
        "a note was addressed to someone"
    );
    assert!(space.answers("pane-lead").is_empty());
    assert_eq!(space.answers("pane-reviewer").len(), 1);

    space.notice_stop(
        "notice-turn-1",
        "background-1",
        &["background-2"],
        "Tests next.",
    );
    let notes = space.notes();
    assert_eq!(notes.len(), 2, "a second hold posted no note: {notes:?}");
    assert_eq!(notes[1][3], "Tests next.");
    assert!(space.dispositions().is_empty());

    space.notice_stop("notice-turn-2", "background-2", &[], "The build passed.");
    let relay = format!("terminal-relay:{attempt}");
    assert_eq!(
        space.relayed().last().unwrap(),
        &[
            relay.clone(),
            thread,
            question_id.clone(),
            "The build passed.".to_owned()
        ],
        "an earlier note stood in for the answer"
    );
    assert_eq!(space.dispositions(), [vec![attempt, "no_reply".to_owned()]]);
    assert_eq!(space.claims()[0]["state"], json!({"done":"no_replied"}));
    assert_eq!(
        space.wire_message(&question_id)["legs"][0]["stage"],
        json!({"stage":"replied","reply_id":relay})
    );
    assert!(space.wire_message(&relay).get("progress_note").is_none());
}

/// A progress note is for the person. What an agent reads of the thread,
/// the recap in a later delivery and the thread's context snapshot, leaves
/// it out, and the real answer is there once it comes.
#[test]
fn agents_never_read_a_progress_note_and_do_read_the_answer() {
    let space = Space::new();
    let question = space.send_as_person("question", "@reviewer Run the build and report.", None);
    let thread = question["thread_id"].as_str().unwrap().to_owned();
    let question_id = question["message_id"].as_str().unwrap().to_owned();
    let ask = space.herdr.envelope_for("pane-reviewer");
    space.prompt(
        "pane-reviewer",
        "session-reviewer",
        CLAUDE,
        &ask.to_string(),
    );
    space.stop_with(
        "session-reviewer",
        CLAUDE,
        "Decision: wait for NOTE_SENTINEL.",
        true,
    );
    assert_eq!(space.notes().len(), 1);

    space.send_as_person(
        "follow-up",
        "@lead Anything @reviewer missed?",
        Some(&question_id),
    );
    let recap =
        space.herdr.envelope_for("pane-lead")["control"]["untrusted_channel_recap"].to_string();
    assert!(recap.contains("Run the build and report."), "{recap}");
    assert!(
        !recap.contains("NOTE_SENTINEL"),
        "the recap shows the note: {recap}"
    );
    let snapshots = space.rows(&format!(
        "SELECT payload_json FROM thread_context_snapshots
         WHERE thread_id = '{thread}' AND participant_address_id = 'address-lead'"
    ));
    assert_eq!(snapshots.len(), 1, "the lead got no thread snapshot");
    assert!(
        !snapshots[0][0].contains("NOTE_SENTINEL"),
        "the thread snapshot shows the note: {snapshots:?}"
    );

    space.notice_stop("notice-turn", "background-1", &[], "ANSWER_SENTINEL");
    assert_eq!(space.relayed().len(), 1);
    space.send_as_person("again", "@lead Did @reviewer finish?", Some(&question_id));
    let recap =
        space.herdr.envelope_for("pane-lead")["control"]["untrusted_channel_recap"].to_string();
    assert!(recap.contains("ANSWER_SENTINEL"), "{recap}");
    assert!(!recap.contains("NOTE_SENTINEL"), "{recap}");
}

/// A turn that ends waiting on a helper shows the person its final message
/// as a note too. Naming the helper in it sends the helper nothing.
#[test]
fn a_hold_on_a_helper_shows_the_person_a_note_that_reaches_no_agent() {
    let space = Space::new();
    let question = space.send_as_person(
        "question",
        "@reviewer Is option B safe? Ask the lead.",
        None,
    );
    let ask = space.herdr.envelope_for("pane-reviewer");
    let attempt = ask["control"]["attempt_id"].as_str().unwrap().to_owned();
    space.prompt(
        "pane-reviewer",
        "session-reviewer",
        CLAUDE,
        &ask.to_string(),
    );
    dispatch::message(
        "send",
        json!({"request_id":"helper","source":"@lead Is option B safe?",
               "author_kind":"agent_session"}),
        &space.runtime("pane-reviewer"),
    )
    .unwrap();
    space.stop(
        "session-reviewer",
        CLAUDE,
        "I asked @lead and will answer when it replies.",
    );
    assert_eq!(space.claims()[0]["state"], json!({"left":"held"}));
    let notes = space.notes();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(notes[0][2], question["message_id"].as_str().unwrap());
    assert_eq!(
        notes[0][3],
        "I asked @lead and will answer when it replies."
    );
    assert_eq!(space.wire_message(&notes[0][0])["progress_note"], true);
    assert_eq!(
        space.answers("pane-lead"),
        ["Is option B safe?"],
        "the note reached the helper it names"
    );
    assert!(space.dispositions().is_empty());
    assert!(space.unresolved_disposition_warnings().is_empty());

    let answered = dispatch::message(
        "reply",
        bound_reply(
            &space,
            &ask,
            "@operator Yes: the lead says option B is safe.",
        ),
        &space.runtime("pane-reviewer"),
    )
    .unwrap();
    assert_eq!(answered["disposition"], "reply");
    assert_eq!(space.dispositions(), [vec![attempt, "reply".to_owned()]]);
}

/// A held answer to another agent posts nothing in the thread, to save that
/// agent the tokens.
#[test]
fn a_held_answer_to_an_agent_posts_no_note() {
    let space = Space::new();
    space.held_on_first_task();
    assert!(space.notes().is_empty(), "{:?}", space.notes());
    assert!(space.relayed().is_empty());
}

/// Channels tells a held answer from ordinary work, and a warning about a
/// held answer that can never arrive from an ordinary unanswered one, with
/// its cause.
#[test]
fn the_wire_marks_a_held_leg_and_says_why_a_held_answer_was_lost() {
    let space = Space::new();
    space.armed_ask();
    let stage = |space: &Space| {
        space.snapshot()["messages"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|message| message["legs"].as_array().unwrap().clone())
            .rfind(|leg| leg["to"] == "address-reviewer")
            .unwrap()["stage"]
            .clone()
    };
    let working = stage(&space);
    assert_eq!(working["stage"], "working");
    assert!(working.get("answer_held").is_none(), "{working}");

    type Gone = (&'static str, fn(&Space));
    let cases: [Gone; 4] = [
        ("replaced", |space| {
            space.observe_reviewer_session("session-replaced")
        }),
        ("retired", Space::retire_reviewer),
        ("retired", Space::close_reviewer_pane),
        ("ended", |space| space.observe_reviewer_session("")),
    ];
    for (cause, gone) in cases {
        let space = Space::new();
        let ask = space.held_on_first_task();
        space.settle_attempt(&ask);
        let held = stage(&space);
        assert_eq!(
            (&held["stage"], &held["answer_held"]),
            (&json!("working"), &json!(true)),
            "{held}"
        );
        gone(&space);
        let attention = space.snapshot()["attention"].clone();
        let card = attention
            .as_array()
            .unwrap()
            .iter()
            .find(|card| card["source_kind"] == "agent_disposition")
            .unwrap_or_else(|| panic!("{cause}: {attention}"));
        assert_eq!(card["answer_lost"], cause, "{card}");
    }

    let space = Space::new();
    let ask = space.armed_ask();
    space.stop("session-reviewer", CLAUDE, "");
    space.settle_attempt(&ask);
    let attention = space.snapshot()["attention"].clone();
    let card = attention
        .as_array()
        .unwrap()
        .iter()
        .find(|card| card["source_kind"] == "agent_disposition")
        .unwrap_or_else(|| panic!("{attention}"));
    assert!(card.get("answer_lost").is_none(), "{card}");
}

/// The recorded busy turn's typed prompt and the notice Claude Code queued
/// into it, about `task`.
fn typed_turn_and_queued_notice(task: &str) -> (Vec<Value>, String, String) {
    let busy = recorded_turn("prompt-busy", "busy-turn");
    let typed = busy[0]["message"]["content"].as_str().unwrap().to_owned();
    let queued = busy
        .iter()
        .find(|record| record["attachment"]["type"] == "queued_command")
        .unwrap()["attachment"]["prompt"]
        .as_str()
        .unwrap()
        .replace("task-first", task);
    (busy, typed, queued)
}

/// With two deliveries held on different tasks, a notice shows both lines,
/// each naming its task, and the one the notice finished first.
#[test]
fn a_queued_notice_lists_the_delivery_it_finished_first_and_names_each_task() {
    let space = Space::new();
    let first = space.held_on_first_task();
    let second = space.second_ask(&first);
    space.reviewer_prompt("wait-turn", &second.to_string());
    space.append_transcript(&recorded_turn("prompt-wait", "wait-turn"));
    space.reviewer_stop(
        "wait-turn",
        "Started.",
        &["task-first", "task-second"],
        &space.transcript(),
    );
    let (busy, typed, queued) = typed_turn_and_queued_notice("task-second");
    space.reviewer_prompt("busy-turn", &typed);
    space.append_transcript(&busy);
    let context = space
        .reviewer_prompt("busy-turn", &queued)
        .unwrap_or_default();
    let line = |ask: &Value| {
        let attempt = ask["control"]["attempt_id"].as_str().unwrap();
        let head = format!("Namedrop: delivery {attempt} from an earlier turn is still open");
        let at = context
            .find(&head)
            .unwrap_or_else(|| panic!("no line for {attempt}: {context}"));
        (at, context[at..].lines().next().unwrap().to_owned())
    };
    let ((first_at, first_line), (second_at, second_line)) = (line(&first), line(&second));
    assert!(
        second_at < first_at,
        "the finished delivery's line came second: {context}"
    );
    assert!(
        second_line.contains("background task task-second, which this notice says ended"),
        "{second_line}"
    );
    assert!(
        first_line.contains("background task task-first;"),
        "{first_line}"
    );
}

/// A delivery that arrives in a turn a held delivery's notice opened cancels
/// that turn's promise, so its prompt shows the held delivery's line.
#[test]
fn a_delivery_in_a_turn_a_notice_opened_shows_the_held_deliverys_line() {
    let space = Space::new();
    let first = space.held_on_first_task();
    let attempt = first["control"]["attempt_id"].as_str().unwrap().to_owned();
    let way_back = format!("Namedrop: delivery {attempt} from an earlier turn is still open");
    let notice = recorded_turn("prompt-notice", "notice-turn");
    let text = notice[0]["message"]["content"]
        .as_str()
        .unwrap()
        .replace("task-second", "task-first");
    let promised = space
        .reviewer_prompt("notice-turn", &text)
        .unwrap_or_default();
    assert!(!promised.contains(&way_back), "{promised}");
    let second = space.second_ask(&first);
    let context = space
        .reviewer_prompt("notice-turn", &second.to_string())
        .unwrap();
    assert!(
        context.contains(&way_back),
        "the delivery's prompt hid the held line: {context}"
    );
}

/// A draft ahead of that delivery keeps it explicit, so it arms no claim. The
/// turn is still about the delivery: its prompt shows the held delivery's
/// line, and the turn's final message never answers the held one.
#[test]
fn a_draft_and_a_delivery_in_a_turn_a_notice_opened_show_the_held_deliverys_line() {
    let space = Space::new();
    let first = space.held_on_first_task();
    let attempt = first["control"]["attempt_id"].as_str().unwrap().to_owned();
    let way_back = format!("Namedrop: delivery {attempt} from an earlier turn is still open");
    let notice = recorded_turn("prompt-notice", "notice-turn");
    let text = notice[0]["message"]["content"]
        .as_str()
        .unwrap()
        .replace("task-second", "task-first");
    let promised = space
        .reviewer_prompt("notice-turn", &text)
        .unwrap_or_default();
    assert!(!promised.contains(&way_back), "{promised}");
    let second = space.second_ask(&first);
    let context = space
        .reviewer_prompt(
            "notice-turn",
            &format!(
                "A draft I had not sent. {}",
                space.as_typed(&second.to_string())
            ),
        )
        .unwrap();
    assert!(
        context.contains("already in this pane's input"),
        "{context}"
    );
    assert_eq!(
        space.claims().len(),
        1,
        "the drafted delivery armed a claim"
    );
    assert!(
        context.contains(&way_back),
        "the drafted delivery's prompt hid the held line: {context}"
    );
    space.append_transcript(&notice);
    space.reviewer_stop("notice-turn", "ping", &[], &space.transcript());
    assert!(
        space.answers("pane-lead").is_empty(),
        "the turn's final message answered the held delivery: {:?}",
        space.answers("pane-lead")
    );
    assert_eq!(space.claims()[0]["state"], json!({"left":"held"}));
}

/// A turn whose first prompt the hook never recorded is still not opened by
/// the notice queued into it: the transcript shows the turn began earlier, so
/// the notice promises nothing and its prompt shows the held line.
#[test]
fn a_queued_notice_in_a_turn_whose_first_prompt_went_unrecorded_promises_nothing() {
    let space = Space::new();
    let first = space.held_on_first_task();
    let attempt = first["control"]["attempt_id"].as_str().unwrap().to_owned();
    let (busy, _, queued) = typed_turn_and_queued_notice("task-first");
    space.append_transcript(&busy);
    let context = space
        .reviewer_prompt("busy-turn", &queued)
        .unwrap_or_default();
    assert!(
        context.contains(&format!(
            "Namedrop: delivery {attempt} from an earlier turn is still open"
        )),
        "{context}"
    );
    let missing = space.directory.root.join("no-transcript.jsonl");
    space.reviewer_stop("busy-turn", "SLEPT", &[], &missing);
    assert!(
        space.answers("pane-lead").is_empty(),
        "{:?}",
        space.answers("pane-lead")
    );
}

impl Space {
    /// The attempts of `count` asks the reviewer's turns ended without
    /// answering, oldest first. Each is settled as the agent going idle does,
    /// so the next ask arrives.
    fn asks_left_open(&self, count: usize) -> Vec<String> {
        (0..count)
            .map(|index| {
                dispatch::message(
                    "send",
                    json!({"request_id":format!("left-{index}"),
                           "source":"@reviewer Reply with the single word pong.",
                           "author_kind":"agent_session"}),
                    &self.runtime("pane-lead"),
                )
                .unwrap();
                let ask = self.herdr.envelope_for("pane-reviewer");
                let turn = format!("left-turn-{index}");
                let turn = ("prompt_id", turn.as_str());
                self.prompt("pane-reviewer", "session-reviewer", turn, &ask.to_string());
                self.stop("session-reviewer", turn, "  ");
                self.settle_attempt(&ask);
                ask["control"]["attempt_id"].as_str().unwrap().to_owned()
            })
            .collect()
    }

    /// The attempts a `delivery inbox` line from a prompt prints, run through
    /// a shell as the agent runs it. The line names this test process as its
    /// executable, so the built binary stands in for that one word.
    fn run_printed_inbox(&self, line: &str) -> Vec<String> {
        let own = std::env::current_exe().and_then(fs::canonicalize).unwrap();
        let rest = line
            .strip_prefix(&format!("'{}' ", own.display()))
            .unwrap_or_else(|| panic!("the line does not start with its executable: {line}"));
        let output = self
            .hosted("sh")
            .args(["-c", &format!(r#""$0" {rest}"#)])
            .arg(env!("CARGO_BIN_EXE_namedrop"))
            .output()
            .unwrap();
        assert!(output.status.success(), "{line}: {output:?}");
        let printed: Value = serde_json::from_slice(&output.stdout).unwrap();
        printed["data"]["envelopes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|envelope| {
                envelope["control"]["attempt_id"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect()
    }
}

/// The line `context` gives for printing the deliveries it shows no line for:
/// the one under the sentence that counts them.
fn printed_inbox_line(context: &str) -> &str {
    context
        .lines()
        .skip_while(|line| !line.contains(" still open, with no line here."))
        .nth(1)
        .unwrap_or_else(|| panic!("no line prints the rest: {context}"))
}

/// The attempts `context` shows a settling line for, in its order.
fn left_open_shown(context: &str) -> Vec<&str> {
    context
        .lines()
        .filter_map(|line| {
            let (attempt, _) = line
                .strip_prefix("Namedrop: delivery ")?
                .split_once(" from an earlier turn is still open")?;
            Some(attempt)
        })
        .collect()
}

/// Claude Code keeps hook context whole only under 10,000 UTF-16 units. Every
/// delivery left open has a line of some 500, so a prompt shows the oldest
/// few, counts the rest and gives the line that prints them. A delivery's own
/// settling lines always come through.
#[test]
fn many_deliveries_left_open_stay_within_what_the_host_keeps_whole() {
    let space = Space::new();
    let left = space.asks_left_open(24);
    let distinct: std::collections::BTreeSet<_> = left.iter().collect();
    assert_eq!(distinct.len(), 24, "{left:?}");
    let whole = |context: &str| {
        let size = context.encode_utf16().count();
        assert!(size < 10_000, "{size} units: {context}");
        assert_eq!(left_open_shown(context), left[..3], "{context}");
        assert!(
            context.contains(
                "Namedrop: 21 more deliveries from earlier turns are still open, with no line here."
            ),
            "{context}"
        );
        let printed = space.run_printed_inbox(printed_inbox_line(context));
        for attempt in &left[3..] {
            assert!(printed.contains(attempt), "{attempt} is not in {printed:?}");
        }
    };

    whole(&space.prompt(
        "pane-reviewer",
        "session-reviewer",
        ("prompt_id", "typed-turn"),
        "Where were we?",
    ));
    dispatch::message(
        "send",
        json!({"request_id":"ask-new","source":"@reviewer Reply with the single word ping.",
               "author_kind":"agent_session"}),
        &space.runtime("pane-lead"),
    )
    .unwrap();
    let ask = space.herdr.envelope_for("pane-reviewer");
    let attempt = ask["control"]["attempt_id"].as_str().unwrap();
    assert!(!left.iter().any(|left| left == attempt), "{attempt}");
    let context = space.prompt(
        "pane-reviewer",
        "session-reviewer",
        ("prompt_id", "delivery-turn"),
        &format!(
            "A draft I had not sent. {}",
            space.as_typed(&ask.to_string())
        ),
    );
    whole(&context);
    for token in ["reply", "complete.no-reply"] {
        let line = context
            .lines()
            .find(|line| line.starts_with(&format!("{token}: ")))
            .unwrap_or_else(|| panic!("the delivery's {token} line is gone: {context}"));
        assert!(
            line.contains(&format!(r#""attempt_id":"{attempt}""#)),
            "{line}"
        );
    }
}

/// A session that resumed in another pane is given the line for the pane its
/// prompt came from. The pane its deliveries reached no longer holds it.
#[test]
fn the_line_that_prints_the_rest_names_the_pane_the_prompt_came_from() {
    let space = Space::new();
    space.asks_left_open(4);
    let context = space.prompt(
        "pane-moved",
        "session-reviewer",
        ("prompt_id", "typed-turn"),
        "Where were we?",
    );
    let inbox = printed_inbox_line(&context);
    assert!(inbox.contains(" --herdr-pane 'pane-moved' "), "{inbox}");
}

/// The pointer the pane holds for its latest delivery.
fn typed_pointer(space: &Space, pane: &str) -> String {
    let prompts = space.herdr.prompts.lock().unwrap();
    let (_, typed) = prompts
        .iter()
        .rev()
        .find(|(to, _)| to == pane)
        .expect("nothing was typed into the pane");
    assert!(typed.starts_with("Namedrop message "), "{typed}");
    typed.clone()
}

fn settle_commands(context: &str) -> usize {
    context
        .lines()
        .filter(|line| line.starts_with("reply: ") || line.starts_with("complete.no-reply: "))
        .count()
}

#[test]
fn a_pointer_delivery_arms_and_its_final_message_settles_with_no_settle_command() {
    let space = Space::new();
    let ask = space.armed_ask();
    let attempt = ask["control"]["attempt_id"].as_str().unwrap().to_owned();
    // `armed_ask` submitted the pointer; the body was typed, the ids were not.
    let typed = typed_pointer(&space, "pane-reviewer");
    assert!(!typed.contains(&attempt), "{typed}");
    assert!(!typed.contains("trusted_instruction"), "{typed}");
    assert_eq!(space.claims()[0]["attempt_id"], attempt);
    assert_eq!(space.claims()[0]["state"], "open");

    space.stop("session-reviewer", CLAUDE, "pong");

    assert_eq!(
        space.dispositions(),
        [vec![attempt.clone(), "reply".to_owned()]]
    );
    let answer = space.herdr.envelope_for("pane-lead");
    assert_eq!(answer["untrusted_body"], "pong");
    let answer_typed = typed_pointer(&space, "pane-lead");
    let context = space.prompt("pane-lead", "session-lead", CODEX, &answer_typed);
    assert_eq!(settle_commands(&context), 1, "{context}");
    assert!(context.contains("\nreply: "), "{context}");
    assert!(
        context.contains("this answers your message, so Namedrop closes it"),
        "{context}"
    );
}

#[test]
fn an_armed_pointer_details_omit_the_rule_its_guidance_replaces() {
    let space = Space::new();
    space.armed_ask();
    space.stop("session-reviewer", CLAUDE, "pong");
    let answer = typed_pointer(&space, "pane-lead");
    let context = space.prompt("pane-lead", "session-lead", CLAUDE, &answer);
    assert!(
        context.contains("Namedrop delivery details: trusted; the prompt's message"),
        "{context}"
    );
    assert!(!context.contains("reply_mode"), "{context}");
    assert!(!context.contains("Registered profile"), "{context}");
    assert!(!context.contains("Finish once"), "{context}");
    assert!(!context.contains("Settle only through"), "{context}");
}

#[test]
fn a_held_pointer_turn_leaves_the_delivery_open_and_shows_the_way_back() {
    let space = Space::new();
    let ask = space.ask_without_prompting();
    let armed = space.prompt(
        "pane-reviewer",
        "session-reviewer",
        CLAUDE,
        &ask.to_string(),
    );
    assert!(
        armed.contains(
            "If it ends while background work you started this turn runs, Namedrop holds your \
             final message, and your final message in the turn that work's notice starts is the \
             reply. If the notice arrives during another turn, or another delivery reaches the \
             turn it starts, answer with the line Namedrop shows."
        ),
        "{armed}"
    );
    let attempt = ask["control"]["attempt_id"].as_str().unwrap().to_owned();
    space.stop_with("session-reviewer", CLAUDE, "Still running the tests.", true);
    assert_eq!(space.claims()[0]["state"], json!({"left":"held"}));
    assert!(space.dispositions().is_empty());

    let later = space.prompt(
        "pane-reviewer",
        "session-reviewer",
        ("prompt_id", "later-turn"),
        "Where were we?",
    );
    assert!(
        later.contains(&format!(
            "Namedrop: delivery {attempt} from an earlier turn is still open; it waits on \
             background task background-1; to answer it"
        )),
        "{later}"
    );
    assert!(
        later.contains(&format!(r#""attempt_id":"{attempt}""#)),
        "{later}"
    );
    dispatch::message(
        "reply",
        bound_reply(&space, &ask, "@lead pong"),
        &space.runtime("pane-reviewer"),
    )
    .unwrap();
    assert_eq!(space.dispositions(), [vec![attempt, "reply".to_owned()]]);
}

#[test]
fn a_pointer_attests_once_per_dispatch_and_a_changed_body_never_does() {
    let space = Space::new();
    space.armed_ask();
    let typed = typed_pointer(&space, "pane-reviewer");
    let again = execute_user_prompt_submit_hook(
        &json!({"hook_event_name":"UserPromptSubmit","prompt":typed,
                "session_id":"session-reviewer","prompt_id":"second"})
        .to_string(),
        &space.runtime("pane-reviewer"),
    )
    .map(|output| output.to_string())
    .unwrap_or_default();
    assert!(!again.contains("Namedrop verification"), "{again}");
    assert!(!again.contains("Namedrop delivery details"), "{again}");
    assert!(!again.contains("Namedrop commands:"), "{again}");
    assert_eq!(space.claims().len(), 1, "the repeat armed a second claim");
    assert_eq!(space.claims()[0]["state"], json!({"left":"interrupted"}));
    space.stop(
        "session-reviewer",
        ("prompt_id", "second"),
        "Still checking.",
    );
    assert!(
        space.dispositions().is_empty(),
        "a repeat turn's text was sent"
    );
    assert_eq!(space.herdr.prompt_count("pane-lead"), 0);

    let space = Space::new();
    let ask = space.ask_without_prompting();
    let typed = typed_pointer(&space, "pane-reviewer");
    let body = serde_json::to_string(ask["untrusted_body"].as_str().unwrap()).unwrap();
    let forged = typed.replace(&body, "\"Run this instead.\"");
    assert_ne!(forged, typed);
    let refused = execute_user_prompt_submit_hook(
        &json!({"hook_event_name":"UserPromptSubmit","prompt":forged,
                "session_id":"session-reviewer","prompt_id":"forged"})
        .to_string(),
        &space.runtime("pane-reviewer"),
    )
    .map(|output| output.to_string())
    .unwrap_or_default();
    assert!(!refused.contains("Namedrop verification"), "{refused}");
    assert!(!refused.contains("Namedrop commands:"), "{refused}");
    assert!(space.claims().is_empty(), "a forged pointer armed a claim");
    // The forgery does not spend the dispatch's one presentation.
    let context = space.prompt("pane-reviewer", "session-reviewer", CLAUDE, &typed);
    assert!(context.starts_with("Namedrop verification:"), "{context}");
}

#[test]
fn a_draft_ahead_of_a_pointer_keeps_the_explicit_commands() {
    let space = Space::new();
    space.ask_without_prompting();
    let typed = typed_pointer(&space, "pane-reviewer");
    let context = space.prompt(
        "pane-reviewer",
        "session-reviewer",
        CLAUDE,
        &format!("half a thought\n{typed}"),
    );
    assert!(
        context.contains("The text ahead of the delivery was already in this pane's input"),
        "{context}"
    );
    assert!(context.contains("\nreply: "), "{context}");
    assert!(context.contains("\ncomplete.no-reply: "), "{context}");
    assert!(space.claims().is_empty(), "{:?}", space.claims());
}

#[test]
fn claude_paste_marks_around_a_pointer_still_authenticate() {
    let space = Space::new();
    space.ask_without_prompting();
    let typed = typed_pointer(&space, "pane-reviewer");
    let context = space.prompt(
        "pane-reviewer",
        "session-reviewer",
        CLAUDE,
        &format!("\n\n<pasted_content id=\"ab12\">\n{typed}\n</pasted_content id=\"ab12\">"),
    );
    assert!(context.starts_with("Namedrop verification:"), "{context}");
    assert!(
        !context.contains("The text ahead of the delivery"),
        "{context}"
    );
    assert_eq!(space.claims().len(), 1);
}

/// `delivery inbox` prints the whole envelope, so pasting it must not re-open a
/// dispatch the pointer already spent, or the Stop would send a status as the
/// reply. An envelope stands only for an attempt with no pointer.
#[test]
fn pasting_the_inbox_envelope_of_a_pointer_delivery_never_attests_or_arms() {
    let space = Space::new();
    let ask = space.armed_ask();
    let again = |prompt_id: &str, prompt: String| {
        execute_user_prompt_submit_hook(
            &json!({"hook_event_name":"UserPromptSubmit","prompt":prompt,
                    "session_id":"session-reviewer","prompt_id":prompt_id})
            .to_string(),
            &space.runtime("pane-reviewer"),
        )
        .map(|output| output.to_string())
        .unwrap_or_default()
    };
    assert_eq!(
        space.inbox("pane-reviewer")[0]["control"]["attempt_id"],
        ask["control"]["attempt_id"]
    );

    let context = again("pasted-turn", ask.to_string());
    assert!(!context.contains("Namedrop verification"), "{context}");
    assert!(!context.contains("Namedrop commands:"), "{context}");
    assert_eq!(space.claims().len(), 1, "the paste armed a second claim");
    assert_eq!(space.claims()[0]["state"], json!({"left":"interrupted"}));
    space.stop(
        "session-reviewer",
        ("prompt_id", "pasted-turn"),
        "Still checking.",
    );
    assert!(
        space.dispositions().is_empty(),
        "a status went out as the reply"
    );
    assert_eq!(space.herdr.prompt_count("pane-lead"), 0);

    let space = Space::new();
    let ask = space.ask_without_prompting();
    let typed = typed_pointer(&space, "pane-reviewer");
    let input = json!({"hook_event_name":"UserPromptSubmit","prompt":ask.to_string(),
                       "session_id":"session-reviewer","prompt_id":"never-presented"});
    let output =
        execute_user_prompt_submit_hook(&input.to_string(), &space.runtime("pane-reviewer"))
            .map(|output| output.to_string())
            .unwrap_or_default();
    assert!(!output.contains("Namedrop verification"), "{output}");
    assert!(space.claims().is_empty(), "the paste armed a claim");
    let context = space.prompt("pane-reviewer", "session-reviewer", CLAUDE, &typed);
    assert!(
        context.starts_with("Namedrop verification:"),
        "the paste spent the pointer: {context}"
    );
}

/// An envelope 1.6.0 typed may quote a pointer header in its body. It is still
/// that envelope, and a person's sentence that quotes a header is still theirs.
#[test]
fn a_quoted_header_neither_hides_an_envelope_nor_swallows_a_mention() {
    let first = Space::new();
    first.ask_without_prompting();
    let header = typed_pointer(&first, "pane-reviewer");
    let space = Space::new();
    dispatch::message(
        "send",
        json!({"request_id":"ask","source":format!("@reviewer What is this?\n{header}"),
               "author_kind":"agent_session"}),
        &space.runtime("pane-lead"),
    )
    .unwrap();
    let ask = space.herdr.envelope_for("pane-reviewer");
    rusqlite::Connection::open(space.directory.state.join("namedrop.sqlite3"))
        .unwrap()
        .execute("UPDATE delivery_attempt_state SET pointer_nonce = NULL", [])
        .unwrap();
    let context = space.submitted(
        "pane-reviewer",
        "session-reviewer",
        CLAUDE,
        &ask.to_string(),
    );
    assert!(context.starts_with("Namedrop verification:"), "{context}");
    assert!(!context.contains("Namedrop delivery details"), "{context}");

    let space = Space::new();
    let output = execute_user_prompt_submit_hook(
        &json!({"hook_event_name":"UserPromptSubmit",
                "prompt":format!("@reviewer what is this? {}", header.lines().next().unwrap())})
        .to_string(),
        &space.runtime("pane-lead"),
    )
    .unwrap();
    assert_eq!(
        output["decision"], "block",
        "the Mention stayed local: {output}"
    );
    assert_eq!(
        space
            .rows("SELECT message_id FROM collaboration_messages")
            .len(),
        1
    );
}

#[test]
fn explicit_pointer_details_name_the_reply_mode_the_settle_rule_uses() {
    let space = Space::new();
    space.ask_without_prompting();
    let typed = typed_pointer(&space, "pane-reviewer");
    let context = space.unkeyed_prompt("pane-reviewer", &typed);
    assert!(context.contains("Finish once"), "{context}");
    assert!(
        context.contains("\nThis delivery: reply_mode agent_thread; thread_id "),
        "{context}"
    );
    assert!(context.contains("\nreply: "), "{context}");
}

#[test]
#[should_panic(expected = "already presented")]
fn a_test_cannot_repeat_a_delivery_by_accident() {
    let space = Space::new();
    let ask = space.armed_ask();
    space.prompt(
        "pane-reviewer",
        "session-reviewer",
        CLAUDE,
        &ask.to_string(),
    );
}

fn session_start(space: &Space, pane: &str, source: &str) -> String {
    execute_session_start_hook(
        &json!({"hook_event_name":"SessionStart","source":source}).to_string(),
        &space.runtime(pane),
    )
    .expect("the policy always arrives")["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// A pointer's details live only in hook context, so a summary can lose them. An
/// explicit delivery arms no claim, and its turn may never come back by itself.
#[test]
fn a_rebuilt_context_is_told_which_deliveries_are_still_open_and_how_to_read_them() {
    let space = Space::new();
    let ask = space.ask_without_prompting();
    let attempt = ask["control"]["attempt_id"].as_str().unwrap().to_owned();
    let typed = typed_pointer(&space, "pane-reviewer");
    space.unkeyed_prompt("pane-reviewer", &typed);
    assert!(space.claims().is_empty(), "the delivery should be explicit");

    for source in ["compact", "resume", "clear"] {
        let context = session_start(&space, "pane-reviewer", source);
        assert!(
            context.starts_with("Namedrop selection policy"),
            "{context}"
        );
        assert!(
            context.contains(&format!("{attempt} from @lead")),
            "{source}: {context}"
        );
        assert!(context.contains(" delivery inbox "), "{source}: {context}");
    }
    assert!(!session_start(&space, "pane-reviewer", "startup").contains(&attempt));
    assert!(!session_start(&space, "pane-lead", "compact").contains(&attempt));

    dispatch::message(
        "reply",
        bound_reply(&space, &ask, "@lead pong"),
        &space.runtime("pane-reviewer"),
    )
    .unwrap();
    assert!(!session_start(&space, "pane-reviewer", "compact").contains(&attempt));
}

/// A Claude Code turn that ends while its background agents run leaves Herdr
/// reading the pane as working and sends no status change. The Stop itself
/// starts the drain, which types the queued mail into the empty prompt box.
#[test]
fn a_claude_stop_with_background_agents_running_takes_mail_queued_during_the_turn() {
    let space = Space::new();
    fs::write(space.directory.root.join("reviewer-status"), "working").unwrap();
    fs::write(
        space.directory.root.join("reviewer-screen"),
        include_str!("fixtures/between-turns/claude-2.1.322-waiting-empty.ansi"),
    )
    .unwrap();
    let summary: Value = serde_json::from_str(
        include_str!("fixtures/final-reply/claude-2.1.319-blocked-stop.jsonl")
            .lines()
            .nth(2)
            .unwrap(),
    )
    .unwrap();
    space.append_transcript(&[]);
    let stop = || {
        let mut command = space.command();
        command.env("HERDR_WORKSPACE_ID", "workspace-1");
        let mut stop = space.start_installed_stop_in(
            command,
            json!({"hook_event_name":"Stop","session_id":"session-reviewer",
                   "prompt_id":"reviewer-turn","last_assistant_message":"Started the helper.",
                   "transcript_path":space.transcript(),
                   "background_tasks":[{"id":"helper","status":"running"}]}),
        );
        stop.output();
        // Claude Code records that its Stop hooks let the turn end.
        space.append_transcript(std::slice::from_ref(&summary));
        stop.wait();
    };
    // A Stop with nothing queued starts no drain.
    stop();
    assert!(!space.directory.root.join("herdr-prompts").exists());
    // The person starts the next turn, and mail is queued while it runs.
    namedrop::turn_phase::record(
        &space.directory.state,
        "socket-a",
        "session-reviewer",
        namedrop::turn_phase::TurnEdge::Prompt,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64,
    );
    // Sent through the binary, so the mail's age is on the clock its drains read.
    let sent = space
        .command()
        .arg("--plugin-state")
        .arg(&space.directory.state)
        .args([
            "send",
            "--herdr-socket",
            "socket-a",
            "--herdr-workspace",
            "workspace-1",
        ])
        .args(["--herdr-pane", "pane-lead", "--input-json"])
        .arg(json!({"request_id":"ask","source":"@reviewer Is the helper done?"}).to_string())
        .output()
        .unwrap();
    assert!(sent.status.success(), "{sent:?}");
    assert!(space.prompted_by_binary("pane-reviewer").is_empty());

    stop();
    // The send's own mailbox worker may still hold the space's queue and run
    // the wake the Stop queued after the Stop has returned.
    let until = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while space.prompted_by_binary("pane-reviewer").is_empty() && std::time::Instant::now() < until
    {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let typed = space.prompted_by_binary("pane-reviewer");
    assert_eq!(typed.len(), 1, "{typed:?}");
    assert_eq!(typed[0]["untrusted_body"], "Is the helper done?");
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

impl Space {
    fn reviewer_phase(&self) -> Option<namedrop::turn_phase::TurnPhase> {
        namedrop::turn_phase::read(&self.directory.state, "socket-a", "session-reviewer")
    }

    fn reviewer_edge(&self, edge: namedrop::turn_phase::TurnEdge, at_ms: u64) {
        namedrop::turn_phase::record(
            &self.directory.state,
            "socket-a",
            "session-reviewer",
            edge,
            at_ms,
        );
    }

    /// The installed prompt hook as Claude Code runs it in the reviewer's pane.
    fn start_reviewer_prompt_in(&self, command: Command, prompt: &str) -> RunningHook {
        self.append_transcript(&[]);
        self.start_installed_hook_in(
            command,
            "user-prompt-submit",
            json!({"hook_event_name":"UserPromptSubmit","session_id":"session-reviewer",
                   "prompt_id":format!("prompt-{}", now_ms()),"prompt":prompt,
                   "transcript_path":self.transcript()}),
        )
    }
}

fn between_turns(phase: Option<namedrop::turn_phase::TurnPhase>) -> bool {
    matches!(phase, Some(namedrop::turn_phase::TurnPhase::Stopped { .. }))
}

/// A leading mention Namedrop sends is blocked as a prompt and starts no turn,
/// so a session between turns, its background agents still running, keeps
/// taking mail.
#[test]
fn a_mention_namedrop_sends_keeps_the_session_between_turns() {
    let space = Space::new();
    space.reviewer_edge(namedrop::turn_phase::TurnEdge::Stop, now_ms() - 1_000);
    let mut command = space.command();
    command.env("CLAUDE_CODE_SESSION_ID", "session-reviewer");
    let mut hook = space.start_reviewer_prompt_in(command, "@lead Is the build green?");
    let output: Value = serde_json::from_str(&hook.output()).unwrap();
    assert_eq!(output["decision"], "block", "{output}");
    // The hook knows its own block; it needs no record of it in the transcript.
    hook.wait();
    assert!(
        between_turns(space.reviewer_phase()),
        "{:?}",
        space.reviewer_phase()
    );
    // Claude Code puts the blocked mention back into the prompt box, so the
    // hook keeps its text for delivery to tell it from a draft and clear it.
    assert_eq!(
        namedrop::turn_phase::restored(&space.directory.state, "socket-a", "session-reviewer")
            .as_deref(),
        Some("@leadIsthebuildgreen?")
    );
}

/// Claude Code 2.1.322 puts a prompt a hook blocked back into its prompt box,
/// so a mention the person typed in the pane stays there after Namedrop sent
/// it. Namedrop deletes it, then types the answer into the empty box as a
/// prompt of its own.
#[test]
fn the_answer_to_a_mention_typed_in_a_claude_pane_goes_into_a_box_cleared_of_the_mention() {
    let space = Space::new();
    let mut command = space.command();
    command.env("CLAUDE_CODE_SESSION_ID", "session-reviewer");
    let mut hook = space.start_reviewer_prompt_in(command, "@lead Is the build green?");
    let output: Value = serde_json::from_str(&hook.output()).unwrap();
    assert_eq!(output["decision"], "block", "{output}");
    hook.wait();
    // The box as Claude Code draws it with the mention back in it.
    let drawn: Vec<&str> = include_str!("fixtures/between-turns/claude-2.1.322-waiting-draft.ansi")
        .lines()
        .collect();
    let box_line = drawn
        .iter()
        .rposition(|line| line.starts_with('\u{276f}'))
        .unwrap();
    let screen: Vec<&str> = drawn
        .iter()
        .enumerate()
        .map(|(index, line)| {
            if index == box_line {
                "\u{276f}\u{a0}@lead Is the build green?"
            } else {
                line
            }
        })
        .collect();
    fs::write(
        space.directory.root.join("reviewer-screen"),
        screen.join("\n"),
    )
    .unwrap();
    // The box Claude Code draws once the delete key emptied it.
    fs::write(
        space.directory.root.join("reviewer-screen-cleared"),
        include_str!("fixtures/between-turns/claude-2.1.322-waiting-empty.ansi"),
    )
    .unwrap();

    namedrop::drain_mailboxes(&space.runtime("pane-lead")).unwrap();
    let asked = space.herdr.envelope_for("pane-lead");
    let answered = space
        .command()
        .arg("--plugin-state")
        .arg(&space.directory.state)
        .args([
            "reply",
            "--herdr-socket",
            "socket-a",
            "--herdr-workspace",
            "workspace-1",
        ])
        .args(["--herdr-pane", "pane-lead", "--input-json"])
        .arg(
            json!({"attempt_id":asked["control"]["attempt_id"],
                   "source":"@reviewer Yes, it is green."})
            .to_string(),
        )
        .output()
        .unwrap();
    assert!(answered.status.success(), "{answered:?}");
    // The reply's own mailbox worker may type the answer after the reply
    // has returned.
    let until = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while space.prompted_by_binary("pane-reviewer").is_empty() && std::time::Instant::now() < until
    {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let typed = space.prompted_by_binary("pane-reviewer");
    assert_eq!(typed.len(), 1, "{typed:?}");
    assert_eq!(typed[0]["untrusted_body"], "Yes, it is green.");
    let keys: Vec<Vec<String>> = fs::read_to_string(space.directory.root.join("herdr-keys"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(keys.len(), 1, "{keys:?}");
    assert_eq!(keys[0][0], "pane-reviewer");
    assert!(keys[0][1..].iter().all(|key| key == "ctrl+u"), "{keys:?}");
}

/// Another plugin's prompt hook blocked or stopped the prompt, and Claude Code
/// recorded that in the transcript. Namedrop reads no block from the
/// transcript, so the prompt counts as a started turn: mail waits for the
/// turn's next Stop or for the background agents, never typed into a turn
/// that may be running.
#[test]
fn a_prompt_another_hook_blocks_or_stops_counts_as_a_started_turn() {
    for fixture in [
        include_str!("fixtures/between-turns/claude-2.1.322-prompt-blocked.jsonl"),
        include_str!("fixtures/between-turns/claude-2.1.322-prompt-stopped.jsonl"),
    ] {
        let mut lines = fixture
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap());
        let mut input = lines.next().unwrap();
        let records: Vec<Value> = lines.collect();
        let space = Space::new();
        space.reviewer_edge(namedrop::turn_phase::TurnEdge::Stop, now_ms() - 1_000);
        space.append_transcript(&[]);
        input["session_id"] = json!("session-reviewer");
        input["transcript_path"] = json!(space.transcript());
        let mut hook = space.start_installed_hook_in(space.command(), "user-prompt-submit", input);
        hook.output();
        // Claude Code writes how the prompt's hooks ended once they have all run.
        space.append_transcript(&records);
        hook.wait();
        assert!(
            matches!(
                space.reviewer_phase(),
                Some(namedrop::turn_phase::TurnPhase::Prompted { .. })
            ),
            "{:?}",
            space.reviewer_phase()
        );
    }
}

/// The prompt hook records the turn's start before it asks Herdr anything, so
/// a Herdr that cannot confirm the pane never leaves the last Stop newest and
/// delivery never types into the running turn.
#[test]
fn a_prompt_whose_hook_cannot_check_its_pane_still_starts_the_turn() {
    let space = Space::new();
    space.reviewer_edge(namedrop::turn_phase::TurnEdge::Stop, now_ms() - 1_000);
    let mut command = space.command();
    // Herdr answers the pane check with no shell, so the pane is unverified.
    command.env("HERDR_PANE_ID", "pane-reviewer");
    space
        .start_reviewer_prompt_in(command, "Run the tests.")
        .wait();
    assert!(
        matches!(
            space.reviewer_phase(),
            Some(namedrop::turn_phase::TurnPhase::Prompted { .. })
        ),
        "{:?}",
        space.reviewer_phase()
    );
}

/// Claude Code records how its Stop hooks ended only once the slowest has
/// run, and a slow one may still block and continue the turn. A Stop whose
/// outcome is not known when the worker stops waiting leaves the turn running.
#[test]
fn a_stop_whose_other_hooks_outlast_the_wait_leaves_the_turn_running() {
    let space = Space::new();
    space.reviewer_edge(namedrop::turn_phase::TurnEdge::Prompt, now_ms() - 1_000);
    let recorded: Vec<Value> =
        include_str!("fixtures/final-reply/claude-2.1.319-blocked-stop.jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    space.append_transcript(&[]);
    let mut stop = space.start_installed_stop_with(json!({"hook_event_name":"Stop",
        "session_id":"session-reviewer","prompt_id":"reviewer-turn",
        "transcript_path":space.transcript(),"last_assistant_message":"Started the helper.",
        "background_tasks":[{"id":"helper","status":"running"}]}));
    stop.output();
    // Every hook but the slow one has run; the worker gives up waiting.
    stop.wait();
    assert!(
        matches!(
            space.reviewer_phase(),
            Some(namedrop::turn_phase::TurnPhase::Prompted { .. })
        ),
        "{:?}",
        space.reviewer_phase()
    );
    // The slow hook blocks, and the turn goes on.
    space.append_transcript(&recorded[..2]);
    assert!(!between_turns(space.reviewer_phase()));
}
