//! A delivery settled from the agent's final message: the prompt hook arms a
//! claim and says so, and the Stop runs the bound operation the CLI runs.
#[path = "support/cleanup.rs"]
mod cleanup;
#[path = "support/dispatch.rs"]
mod dispatch;

use namedrop::{
    ActorKind, AgentRuntime, AgentSessionId, Clock, HerdrCommandExecutor, HerdrCommandOutput,
    IdGenerator, LifecycleRuntime, MutationAttribution, PaneAddressDraft, PaneAddressId,
    PaneBindingId, PaneRoleId, StateNamespace, StateStore, complete_capability_profile,
    execute_integration_operation, execute_user_prompt_submit_hook, expand_attempt_reply,
    final_reply::{BoundOperation, FinalText, HostTurn, StopFacts, settle_at_stop},
    issue_registration_challenge,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
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

struct FakeHerdr {
    worktree: String,
    prompts: Mutex<Vec<(String, String)>>,
}

impl FakeHerdr {
    fn envelope_for(&self, pane_id: &str) -> Value {
        let prompts = self.prompts.lock().unwrap();
        let (_, envelope) = prompts
            .iter()
            .rev()
            .find(|(pane, _)| pane == pane_id)
            .unwrap_or_else(|| panic!("no envelope was dispatched to {pane_id}"));
        serde_json::from_str(envelope).unwrap()
    }

    fn prompt_count(&self, pane_id: &str) -> usize {
        let prompts = self.prompts.lock().unwrap();
        prompts.iter().filter(|(pane, _)| pane == pane_id).count()
    }
}

impl HerdrCommandExecutor for FakeHerdr {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let stdout = match args {
            ["workspace", "list"] => {
                json!({"result":{"workspaces":[{"workspace_id":"workspace-1"}]}}).to_string()
            }
            ["worktree", "list", "--cwd", cwd, "--json"] if *cwd == self.worktree => json!({
                "result": {"worktrees": [{"path": self.worktree, "open_workspace_id": "workspace-1"}]}
            })
            .to_string(),
            ["pane", "list", "--workspace", "workspace-1"] => json!({"result": {"panes": [
                {"pane_id":"pane-lead","terminal_id":"t1","tab_id":"tab-1","workspace_id":"workspace-1",
                 "agent":"codex","agent_session":{"value":"session-lead"},"agent_status":"idle"},
                {"pane_id":"pane-reviewer","terminal_id":"t2","tab_id":"tab-2","workspace_id":"workspace-1",
                 "agent":"claude","agent_session":{"value":"session-reviewer"},"agent_status":"idle"}
            ]}})
            .to_string(),
            ["agent", "prompt", pane_id, envelope] => {
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
    herdr: FakeHerdr,
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
        let herdr = FakeHerdr {
            worktree: directory.repo.display().to_string(),
            prompts: Mutex::new(Vec::new()),
        };
        let space = Self {
            directory,
            herdr,
            ids: SequenceIds(AtomicU64::new(1)),
        };
        for (address, host, pane) in [
            ("address-lead", AgentRuntime::Codex, "pane-lead"),
            (
                "address-reviewer",
                AgentRuntime::ClaudeCode,
                "pane-reviewer",
            ),
        ] {
            let runtime = space.runtime(pane);
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
        space
    }

    fn runtime<'a>(
        &'a self,
        pane: &'a str,
    ) -> LifecycleRuntime<'a, FakeHerdr, FixedClock, SequenceIds> {
        LifecycleRuntime {
            state_directory: &self.directory.state,
            home_directory: Some(&self.directory.home),
            agent_config: namedrop::AgentConfigDirectories::default(),
            cwd: &self.directory.repo,
            workspace_id: None,
            current_pane_id: Some(pane),
            session_socket_id: "socket-a",
            herdr: &self.herdr,
            clock: &FixedClock,
            ids: &self.ids,
        }
    }

    /// The prompt hook as the host runs it on a delivery turn: Claude Code
    /// names the turn by `prompt_id`, Codex by `turn_id`.
    fn prompt(&self, pane: &str, session: &str, turn: (&str, &str), prompt: &str) -> String {
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
        let (prompt_id, turn_id) = match turn.0 {
            "prompt_id" => (Some(turn.1), None),
            _ => (None, Some(turn.1)),
        };
        let stop = StopFacts {
            session: session.to_owned(),
            turn: HostTurn::from_ids(prompt_id, turn_id),
            background_running: false,
            text: FinalText::bounded(text),
        };
        settle_at_stop(&self.directory.state, "socket-a", &stop, 42, &mut |bound| {
            self.run(bound)
        });
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

    fn dispositions(&self) -> Vec<Vec<String>> {
        self.rows("SELECT attempt_id, disposition_kind FROM delivery_dispositions ORDER BY occurred_at_ms, rowid")
    }

    fn claims(&self) -> Vec<Value> {
        let mut claims = Vec::new();
        for session in fs::read_dir(self.directory.state.join("final-reply")).unwrap() {
            let file = session.unwrap().path().join("claims.json");
            claims.extend(serde_json::from_slice::<Vec<Value>>(&fs::read(file).unwrap()).unwrap());
        }
        claims
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
            "Namedrop commands: your final message is your reply to @lead. Namedrop sends it when your turn ends, so run no settle command and write no status line. For no reply, run this line first. If background work is still running"
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
    assert!(!context.contains("If background work"), "{context}");

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
        &json!({"hook_event_name":"UserPromptSubmit","prompt":ask.to_string()}).to_string(),
        &space.runtime("pane-reviewer"),
    )
    .unwrap();
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

    let root = &space.directory.root;
    let herdr = root.join("herdr");
    fs::write(
        &herdr,
        format!(
            r#"#!/usr/bin/env python3
import json, sys
args = sys.argv[1:]
if args[:1] == ['--socket']: args = args[2:]
panes = {{'result': {{'panes': [
    {{'pane_id':'pane-lead','terminal_id':'t1','tab_id':'tab-1','workspace_id':'workspace-1','agent':'codex','agent_session':{{'value':'session-lead'}},'agent_status':'idle'}},
    {{'pane_id':'pane-reviewer','terminal_id':'t2','tab_id':'tab-2','workspace_id':'workspace-1','agent':'claude','agent_session':{{'value':'session-reviewer'}},'agent_status':'idle'}}]}}}}
if args[:2] == ['workspace','list']: print(json.dumps({{'result':{{'workspaces':[{{'workspace_id':'workspace-1'}}]}}}}))
elif args[:2] == ['worktree','list']: print(json.dumps({{'result':{{'worktrees':[{{'path':{repo:?},'open_workspace_id':'workspace-1'}}]}}}}))
elif args[:2] == ['pane','list']: print(json.dumps(panes))
else: print('{{}}')
"#,
            repo = space.directory.repo.display().to_string()
        ),
    )
    .unwrap();
    fs::set_permissions(&herdr, fs::Permissions::from_mode(0o700)).unwrap();
    let payload = json!({"hook_event_name":"Stop","session_id":"session-reviewer",
                         "prompt_id":CLAUDE.1,"last_assistant_message":"pong","background_tasks":[]});
    let mut child = Command::new(env!("CARGO_BIN_EXE_namedrop"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &space.directory.home)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("HERDR_SOCKET_PATH", "socket-a")
        .env("HERDR_BIN_PATH", &herdr)
        .env("NAMEDROP_HERDR_TIMEOUT_MS", "250")
        .current_dir(&space.directory.repo)
        .args(["hook", "stop", "--state-directory"])
        .arg(&space.directory.state)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while space.dispositions().is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(space.dispositions(), [vec![attempt, "reply".to_owned()]]);
    assert_eq!(
        space.rows("SELECT body FROM collaboration_messages ORDER BY created_at_ms DESC, rowid DESC LIMIT 1"),
        [vec!["pong".to_owned()]]
    );
    while space.claims()[0]["state"] == json!("open") && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(space.claims()[0]["state"], json!({"done":"replied"}));
}
