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
    execute_integration_operation, execute_lifecycle_operation, execute_message_operation,
    execute_user_prompt_submit_hook, expand_attempt_reply,
    final_reply::{BoundOperation, FinalText, HostTurn, StopFacts, settle_at_stop},
    issue_registration_challenge, send_operator_message,
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
            ["worktree", "list", "--workspace", "workspace-1", "--json"] => json!({
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
        self.stop_with(session, turn, text, false);
    }

    fn stop_with(&self, session: &str, turn: (&str, &str), text: &str, background_running: bool) {
        let stop = stop_facts(session, turn, text, background_running);
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

    /// The prompt hook's context with no session or turn id, which arms
    /// nothing: today's explicit commands.
    fn unkeyed_prompt(&self, pane: &str, prompt: &str) -> String {
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

    fn relayed(&self) -> Vec<Vec<String>> {
        self.rows(
            "SELECT message_id, thread_id, in_reply_to, body FROM collaboration_messages
             WHERE grammar_version='terminal-relay/v1' ORDER BY created_at_ms, rowid",
        )
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
    StopFacts {
        session: session.to_owned(),
        turn: HostTurn::from_ids(prompt_id, turn_id),
        background_running,
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
            "Namedrop commands: your final message is your reply to @lead. Namedrop sends it when your turn ends, so run no settle command and write no status line. If your turn ends while you wait on a helper or background work, Namedrop sends nothing and you reply later. For no reply, run this line first."
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
        let today = space.unkeyed_prompt(pane, &envelope.to_string());
        assert!(
            today.contains("Namedrop commands: the runnable lines this delivery authorized")
                && today.contains(&format!("\n{line}: ")),
            "{shape}: {today}"
        );
        assert_eq!(
            space.prompt(pane, session, turn, &envelope.to_string()),
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

/// One installed Stop hook the host started, and the descriptor whose end of
/// file means the hook and any worker it detached have exited.
struct RunningStop {
    child: std::process::Child,
    done: std::os::unix::net::UnixStream,
}

impl RunningStop {
    fn wait(mut self) {
        assert!(self.child.wait().unwrap().success());
        self.done
            .set_read_timeout(Some(std::time::Duration::from_secs(15)))
            .unwrap();
        assert_eq!(
            std::io::Read::read(&mut self.done, &mut [0]).unwrap(),
            0,
            "the Stop worker outlived its deadline"
        );
    }
}

impl Space {
    /// A Herdr that answers like the space and logs every prompt it is
    /// handed, for the binary's own processes.
    fn herdr_script(&self) -> PathBuf {
        let herdr = self.directory.root.join("herdr");
        fs::write(
            &herdr,
            format!(
                r#"#!/usr/bin/env python3
import json, sys
from pathlib import Path
args = sys.argv[1:]
if args[:1] == ['--socket']: args = args[2:]
panes = {{'result': {{'panes': [
    {{'pane_id':'pane-lead','terminal_id':'t1','tab_id':'tab-1','workspace_id':'workspace-1','agent':'codex','agent_session':{{'value':'session-lead'}},'agent_status':'idle'}},
    {{'pane_id':'pane-reviewer','terminal_id':'t2','tab_id':'tab-2','workspace_id':'workspace-1','agent':'claude','agent_session':{{'value':'session-reviewer'}},'agent_status':'idle'}}]}}}}
if args[:2] == ['workspace','list']: print(json.dumps({{'result':{{'workspaces':[{{'workspace_id':'workspace-1'}}]}}}}))
elif args[:2] == ['worktree','list']: print(json.dumps({{'result':{{'worktrees':[{{'path':{repo:?},'open_workspace_id':'workspace-1'}}]}}}}))
elif args[:2] == ['pane','list']: print(json.dumps(panes))
else:
    if args[:2] == ['agent','prompt']:
        with (Path(__file__).parent / 'herdr-prompts').open('a') as log: log.write(json.dumps(args[2:]) + '\n')
    print('{{}}')
"#,
                repo = self.directory.repo.display().to_string()
            ),
        )
        .unwrap();
        fs::set_permissions(&herdr, fs::Permissions::from_mode(0o700)).unwrap();
        herdr
    }

    /// The binary as a host runs it, against the script Herdr.
    fn command(&self) -> Command {
        let root = &self.directory.root;
        let mut command = Command::new(env!("CARGO_BIN_EXE_namedrop"));
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
            .map(|args| serde_json::from_str(&args[1]).unwrap())
            .collect()
    }

    /// The installed Stop hook as the host runs it. It returns once the hook
    /// and any detached worker have exited.
    fn installed_stop(&self, session: &str, prompt_id: &str, text: &str) {
        self.start_installed_stop(session, prompt_id, text).wait();
    }

    fn start_installed_stop(&self, session: &str, prompt_id: &str, text: &str) -> RunningStop {
        use std::os::{fd::AsRawFd, unix::process::CommandExt};
        let payload = json!({"hook_event_name":"Stop","session_id":session,
                             "prompt_id":prompt_id,"last_assistant_message":text,"background_tasks":[]});
        let (done, lifetime) = std::os::unix::net::UnixStream::pair().unwrap();
        let lifetime_fd = lifetime.as_raw_fd();
        let mut command = self.command();
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
            .env("NAMEDROP_HERDR_TIMEOUT_MS", "250")
            .args(["hook", "stop", "--state-directory"])
            .arg(&self.directory.state)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
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
        RunningStop { child, done }
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
            "\n\nNamedrop commands: your final message goes to the person when your turn ends, so run no settle command and write no status line. If your turn ends while you wait on a helper or background work, Namedrop sends nothing and you reply later."
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
    fn armed_ask(&self) -> Value {
        dispatch::message(
            "send",
            json!({"request_id":"ask","source":"@reviewer Reply with the single word pong.",
                   "author_kind":"agent_session"}),
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
            .map(|(_, envelope)| serde_json::from_str(envelope).unwrap())
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
