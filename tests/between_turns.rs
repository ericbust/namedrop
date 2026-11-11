//! Mail for an agent session whose prompt box the person may be typing in, and
//! for a Claude Code session that finished its turn while its background
//! agents run on. Herdr reads that screen as working; the session's own turn
//! edges say a prompt typed now starts its next turn.
#[path = "support/cleanup.rs"]
mod cleanup;

use namedrop::{
    ActorKind, AgentRuntime, AgentSessionId, Clock, DeliveryId, DrainReport, HealthSeverity,
    HerdrCommandExecutor, HerdrCommandOutput, IdGenerator, LifecycleRuntime, MessageDraft,
    MessageId, MessageRecipientDraft, MutationAttribution, PaneAddressDraft, PaneAddressId,
    PaneBindingId, PaneRoleId, PromptFailureKind, StateNamespace, StateStore, ThreadId,
    complete_capability_profile, diagnose_collaboration, drain_mailboxes,
    execute_integration_operation, issue_registration_challenge, recheck_drafts,
    turn_phase::{TurnEdge, record, record_restored},
};
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

/// The end of Claude Code 2.1.322's screen, as Herdr renders it, after its
/// turn ended with one background agent still running.
const EMPTY_BOX: &str = include_str!("fixtures/between-turns/claude-2.1.322-waiting-empty.ansi");
/// A screen of the same session with a draft the person has not sent.
const DRAFT_BOX: &str = include_str!("fixtures/between-turns/claude-2.1.322-waiting-draft.ansi");
/// Claude Code with the person in shell mode, which they enter by typing `!`.
const SHELL_MODE: &str = include_str!("fixtures/between-turns/claude-2.1.322-idle-shell-mode.ansi");
/// Claude Code with its model picker open: no prompt box on screen.
const MODEL_PICKER: &str =
    include_str!("fixtures/between-turns/claude-2.1.322-idle-model-picker.ansi");
/// The end of Codex 0.167.0's screen, idle, with its box empty and with a draft.
const CODEX_EMPTY: &str = include_str!("fixtures/between-turns/codex-0.167.0-idle-empty.ansi");
const CODEX_DRAFT: &str = include_str!("fixtures/between-turns/codex-0.167.0-idle-draft.ansi");

const SESSION: &str = "session-1";
const QUEUED_AT_MS: u64 = 1_000;

struct Recipient {
    host: &'static str,
    status: Mutex<String>,
    screen: Mutex<Option<String>>,
    screen_reads: AtomicU64,
    pane_lists: AtomicU64,
    prompts: Mutex<Vec<String>>,
    /// The keys each `send-keys` call pressed in the pane.
    keys: Mutex<Vec<Vec<String>>>,
    /// Whether the delete key empties the box, as it does in Claude Code
    /// 2.1.322.
    delete_empties: std::sync::atomic::AtomicBool,
}

impl HerdrCommandExecutor for Recipient {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let stdout = match args {
            ["workspace", "list"] => {
                r#"{"result":{"workspaces":[{"workspace_id":"workspace-1"}]}}"#.to_owned()
            }
            ["worktree", "list", "--cwd", "/repo", "--json"]
            | ["worktree", "list", "--json"]
            | ["worktree", "list", "--workspace", "workspace-1", "--json"] => {
                r#"{"result":{"worktrees":[{"path":"/repo","open_workspace_id":"workspace-1"}]}}"#
                    .to_owned()
            }
            ["pane", "list", "--workspace", "workspace-1"] => {
                self.pane_lists.fetch_add(1, Ordering::Relaxed);
                json!({"result": {"panes": [{
                "pane_id": "pane-recipient",
                "terminal_id": "terminal-recipient",
                "tab_id": "tab-1",
                "workspace_id": "workspace-1",
                "agent": self.host,
                "agent_session": {"value": SESSION},
                "agent_status": self.status.lock().unwrap().clone(),
                }]}})
                .to_string()
            }
            [
                "pane",
                "read",
                "pane-recipient",
                "--source",
                "recent",
                "--lines",
                _,
                "--format",
                "ansi",
            ] => {
                self.screen_reads.fetch_add(1, Ordering::Relaxed);
                self.screen
                    .lock()
                    .unwrap()
                    .clone()
                    .ok_or("the pane could not be read")?
            }
            ["agent", "send-keys", "pane-recipient", keys @ ..] => {
                self.keys
                    .lock()
                    .unwrap()
                    .push(keys.iter().map(|key| (*key).to_owned()).collect());
                if keys.iter().all(|key| *key == "ctrl+u")
                    && self.delete_empties.load(Ordering::Relaxed)
                {
                    *self.screen.lock().unwrap() = Some(EMPTY_BOX.to_owned());
                }
                "{}".to_owned()
            }
            ["agent", "prompt", "pane-recipient", prompt] => {
                self.prompts.lock().unwrap().push((*prompt).to_owned());
                "{}".to_owned()
            }
            ["pane", "report-metadata", ..] => "{}".to_owned(),
            _ => return Err(format!("unexpected Herdr command: {args:?}")),
        };
        Ok(HerdrCommandOutput { stdout })
    }

    fn classify_prompt_failure(&self, _: &str) -> PromptFailureKind {
        PromptFailureKind::AmbiguousInvocation
    }
}

#[derive(Default)]
struct TestClock(AtomicU64);

impl Clock for TestClock {
    fn now_unix_ms(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }

    fn wait_ms(&self, _: u64) {}
}

struct SequenceIds(AtomicU64);

impl IdGenerator for SequenceIds {
    fn next_id(&self, kind: &str) -> String {
        format!("{kind}-{}", self.0.fetch_add(1, Ordering::Relaxed))
    }
}

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    herdr: Recipient,
    clock: TestClock,
    ids: SequenceIds,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        cleanup::remove_fixture(&self.root).unwrap();
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

impl Fixture {
    /// @recipient, a `host` agent Herdr reads as `status`, with one message
    /// from the person queued for it.
    fn new(host: AgentRuntime, status: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "namedrop-between-turns-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("home")).unwrap();
        let fixture = Self {
            herdr: Recipient {
                host: match host {
                    AgentRuntime::ClaudeCode => "claude",
                    AgentRuntime::Codex => "codex",
                },
                status: Mutex::new(status.to_owned()),
                screen: Mutex::new(Some(
                    match host {
                        AgentRuntime::ClaudeCode => EMPTY_BOX,
                        AgentRuntime::Codex => CODEX_EMPTY,
                    }
                    .to_owned(),
                )),
                screen_reads: AtomicU64::new(0),
                pane_lists: AtomicU64::new(0),
                prompts: Mutex::new(Vec::new()),
                keys: Mutex::new(Vec::new()),
                delete_empties: std::sync::atomic::AtomicBool::new(true),
            },
            clock: TestClock::default(),
            ids: SequenceIds(AtomicU64::new(1)),
            home: root.join("home"),
            root,
        };
        let namespace = StateNamespace::for_space("socket-a", "workspace-1");
        let attribution = MutationAttribution::new(ActorKind::Plugin, "test", 10);
        let author = address("address-author", "author");
        let recipient = address("address-recipient", "recipient");
        let thread_id = ThreadId::new("thread-1");
        StateStore::open(fixture.state())
            .unwrap()
            .transaction(|transaction| {
                transaction.activate_worktree(&namespace, &attribution)?;
                transaction.create_pane_address(&namespace, &author, &attribution)?;
                transaction.create_pane_address(&namespace, &recipient, &attribution)?;
                transaction.bind_pane(
                    &namespace,
                    &PaneBindingId::new("binding-recipient"),
                    &recipient.id,
                    "pane-recipient",
                    &attribution,
                )?;
                transaction.observe_agent_session(
                    &namespace,
                    "observation-recipient",
                    &recipient.id,
                    &AgentSessionId::new(SESSION),
                    status,
                    &attribution,
                )?;
                transaction.create_thread(&namespace, &thread_id, "Test thread", &attribution)?;
                transaction.complete_reconciliation(&namespace)?;
                Ok(())
            })
            .unwrap();
        let challenge =
            issue_registration_challenge(&recipient.id, host, &fixture.runtime()).unwrap();
        execute_integration_operation(
            "register",
            json!({"runtime": host, "protocol_version": "collab/v1", "nonce": challenge.nonce,
                   "capabilities": complete_capability_profile()}),
            &fixture.runtime(),
        )
        .unwrap();
        StateStore::open(fixture.state())
            .unwrap()
            .transaction(|transaction| {
                transaction.append_message(
                    &namespace,
                    &MessageDraft {
                        thread_id,
                        message_id: MessageId::new("message-1"),
                        in_reply_to: None,
                        author_address_id: author.id,
                        author_kind: ActorKind::Human,
                        author_alias_snapshot: "author".to_owned(),
                        author_agent_session_snapshot: None,
                        request_id: "request-1".to_owned(),
                        authored_source: "@recipient Are the tests green?".to_owned(),
                        body: "Are the tests green?".to_owned(),
                        grammar_version: "mention/v1".to_owned(),
                        recipients: vec![MessageRecipientDraft {
                            address_id: recipient.id,
                            alias_snapshot: "recipient".to_owned(),
                            agent_session_snapshot: Some(AgentSessionId::new(SESSION)),
                            delivery_id: DeliveryId::new("delivery-1"),
                        }],
                        context_references: Vec::new(),
                    },
                    &MutationAttribution::new(ActorKind::Human, "test", QUEUED_AT_MS),
                )
            })
            .unwrap();
        fixture.clock.0.store(QUEUED_AT_MS, Ordering::Relaxed);
        fixture
    }

    fn state(&self) -> &Path {
        &self.root
    }

    fn runtime(&self) -> LifecycleRuntime<'_, Recipient, TestClock, SequenceIds> {
        LifecycleRuntime {
            state_directory: self.state(),
            home_directory: Some(&self.home),
            agent_config: namedrop::AgentConfigDirectories::default(),
            cwd: Path::new("/repo"),
            workspace_id: None,
            current_pane_id: Some("pane-recipient"),
            session_socket_id: "socket-a",
            herdr: &self.herdr,
            clock: &self.clock,
            ids: &self.ids,
        }
    }

    fn edge(&self, edge: TurnEdge, at_ms: u64) {
        record(self.state(), "socket-a", SESSION, edge, at_ms);
    }

    fn screen(&self, screen: Option<&str>) {
        *self.herdr.screen.lock().unwrap() = screen.map(str::to_owned);
    }

    /// The bodies one drain typed into the pane.
    fn drain(&self) -> Vec<String> {
        self.drain_report().1
    }

    /// One drain's report, and the bodies it typed into the pane.
    fn drain_report(&self) -> (DrainReport, Vec<String>) {
        let report = drain_mailboxes(&self.runtime()).unwrap();
        (report, self.typed())
    }

    /// The mailbox worker's recheck of the boxes `held` names, and the bodies
    /// it typed into the pane.
    fn recheck(&self, held: &DrainReport) -> (DrainReport, Vec<String>) {
        let report = recheck_drafts(
            &LifecycleRuntime {
                workspace_id: Some("workspace-1"),
                ..self.runtime()
            },
            held.draft_held.clone(),
        )
        .unwrap();
        (report, self.typed())
    }

    fn typed(&self) -> Vec<String> {
        std::mem::take(&mut *self.herdr.prompts.lock().unwrap())
    }

    fn set_status(&self, status: &str) {
        *self.herdr.status.lock().unwrap() = status.to_owned();
    }

    fn long_wait(&self) -> Option<namedrop::HealthIssue> {
        self.clock.0.store(QUEUED_AT_MS + 61_000, Ordering::Relaxed);
        diagnose_collaboration(&self.runtime())
            .issues
            .into_iter()
            .find(|issue| issue.code == "ND-DELIVERY-005")
    }
}

#[test]
fn a_claude_session_that_ended_its_turn_takes_mail_while_its_background_agents_run() {
    let fixture = Fixture::new(AgentRuntime::ClaudeCode, "working");
    fixture.edge(TurnEdge::Prompt, 2_000);
    fixture.edge(TurnEdge::Stop, 3_000);
    let typed = fixture.drain();
    assert_eq!(typed.len(), 1, "{typed:?}");
    assert!(typed[0].contains("Are the tests green?"), "{typed:?}");
}

#[test]
fn a_turn_still_running_gets_nothing_until_it_ends() {
    let fixture = Fixture::new(AgentRuntime::ClaudeCode, "working");
    fixture.edge(TurnEdge::Stop, 2_000);
    // A task notice, the person, or an earlier delivery started a turn.
    fixture.edge(TurnEdge::Prompt, 3_000);
    assert_eq!(fixture.drain(), Vec::<String>::new());
    // A session whose hooks never recorded an edge is read as Herdr reads it.
    let unrecorded = Fixture::new(AgentRuntime::ClaudeCode, "working");
    assert_eq!(unrecorded.drain(), Vec::<String>::new());
    assert_eq!(unrecorded.herdr.screen_reads.load(Ordering::Relaxed), 0);
    // The same turn's Stop takes the mail.
    fixture.edge(TurnEdge::Stop, 4_000);
    assert_eq!(fixture.drain().len(), 1);
}

#[test]
fn a_session_waiting_on_a_question_gets_nothing_even_after_its_stop() {
    for status in ["blocked", "unknown"] {
        let fixture = Fixture::new(AgentRuntime::ClaudeCode, status);
        fixture.edge(TurnEdge::Stop, 2_000);
        assert_eq!(fixture.drain(), Vec::<String>::new(), "{status}");
    }
}

#[test]
fn a_draft_or_an_unreadable_prompt_box_keeps_the_mail_queued() {
    let fixture = Fixture::new(AgentRuntime::ClaudeCode, "working");
    fixture.edge(TurnEdge::Stop, 2_000);
    for screen in [Some(DRAFT_BOX), Some("no prompt box drawn here\n"), None] {
        fixture.screen(screen);
        assert_eq!(fixture.drain(), Vec::<String>::new(), "{screen:?}");
    }
    fixture.screen(Some(EMPTY_BOX));
    assert_eq!(fixture.drain().len(), 1);
}

#[test]
fn codex_mail_still_waits_for_herdr_to_read_the_session_idle() {
    let fixture = Fixture::new(AgentRuntime::Codex, "working");
    fixture.edge(TurnEdge::Stop, 2_000);
    assert_eq!(fixture.drain(), Vec::<String>::new());
    assert_eq!(fixture.herdr.screen_reads.load(Ordering::Relaxed), 0);
    *fixture.herdr.status.lock().unwrap() = "idle".to_owned();
    assert_eq!(fixture.drain().len(), 1);
}

#[test]
fn doctor_says_why_mail_has_waited_over_a_minute() {
    let fixture = Fixture::new(AgentRuntime::ClaudeCode, "working");
    fixture
        .clock
        .0
        .store(QUEUED_AT_MS + 59_000, Ordering::Relaxed);
    assert!(
        diagnose_collaboration(&fixture.runtime())
            .issues
            .iter()
            .all(|issue| issue.code != "ND-DELIVERY-005")
    );

    let running = fixture.long_wait().expect("a turn that runs on is shown");
    assert_eq!(
        running.summary,
        "A message to @recipient has waited 1 minute."
    );
    assert_eq!(running.severity, HealthSeverity::Info);
    assert!(running.explanation.contains("working"), "{running:?}");
    assert_eq!(running.diagnostic["held_by"], "status");
    assert_eq!(running.diagnostic["delivery_id"], "delivery-1");

    fixture.edge(TurnEdge::Stop, 2_000);
    fixture.screen(Some(DRAFT_BOX));
    let draft = fixture.long_wait().expect("a draft in the way is shown");
    assert_eq!(draft.severity, HealthSeverity::Notice);
    assert!(draft.explanation.contains("draft"), "{draft:?}");
    assert_eq!(draft.diagnostic["held_by"], "draft");
    assert_eq!(draft.diagnostic["agent_status"], "working");

    fixture.screen(Some(EMPTY_BOX));
    assert_eq!(fixture.long_wait(), None);

    fixture.set_status("idle");
    fixture.screen(Some(DRAFT_BOX));
    let idle_draft = fixture
        .long_wait()
        .expect("a draft in an idle box is shown");
    assert_eq!(idle_draft.diagnostic["held_by"], "draft");
    assert_eq!(idle_draft.diagnostic["agent_status"], "idle");
    fixture.screen(Some(MODEL_PICKER));
    assert_eq!(fixture.long_wait(), None);
}

#[test]
fn an_idle_claude_session_with_a_draft_gets_nothing_typed_into_it() {
    let fixture = Fixture::new(AgentRuntime::ClaudeCode, "idle");
    for screen in [DRAFT_BOX, SHELL_MODE] {
        fixture.screen(Some(screen));
        let (report, typed) = fixture.drain_report();
        assert_eq!(typed, Vec::<String>::new(), "{screen}");
        assert_eq!(report.draft_held.len(), 1, "{report:?}");
        assert_eq!(report.draft_held[0].pane_id, "pane-recipient");
    }
    let done = Fixture::new(AgentRuntime::ClaudeCode, "done");
    done.screen(Some(DRAFT_BOX));
    assert_eq!(done.drain(), Vec::<String>::new());
}

#[test]
fn an_idle_claude_session_whose_draft_fills_the_pane_gets_nothing_typed_into_it() {
    // An 80-line draft, as Claude Code 2.1.322 draws it, in a pane too short
    // for the box: the screen starts inside the box, below its `❯` line.
    let tall = include_str!("fixtures/between-turns/claude-2.1.322-tall-draft.ansi");
    let below_the_top: String = tall
        .lines()
        .skip_while(|line| !line.starts_with('\u{276f}'))
        .skip(1)
        .collect::<Vec<_>>()
        .join("\n");
    for screen in [tall.to_owned(), below_the_top] {
        let fixture = Fixture::new(AgentRuntime::ClaudeCode, "idle");
        fixture.screen(Some(&screen));
        let (report, typed) = fixture.drain_report();
        assert_eq!(typed, Vec::<String>::new(), "{screen}");
        assert_eq!(report.draft_held.len(), 1, "{report:?}");
    }
}

#[test]
fn an_idle_claude_session_with_an_empty_box_takes_the_mail() {
    // The box as Claude Code leaves it after a turn: the suggested next
    // prompt drawn faint, with the cursor on its first letter.
    let suggested = EMPTY_BOX.replace(
        "\u{276f}\u{a0}\u{1b}[0m\u{1b}[7m \u{1b}[0m",
        "\u{276f}\u{a0}\u{1b}[0m\u{1b}[7mr\u{1b}[0m\u{1b}[2mun the tests\u{1b}[0m",
    );
    assert_ne!(suggested, EMPTY_BOX);
    for screen in [EMPTY_BOX.to_owned(), suggested] {
        let fixture = Fixture::new(AgentRuntime::ClaudeCode, "idle");
        fixture.screen(Some(&screen));
        let (report, typed) = fixture.drain_report();
        assert_eq!(typed.len(), 1, "{screen}");
        assert!(typed[0].contains("Are the tests green?"), "{typed:?}");
        assert!(report.draft_held.is_empty(), "{report:?}");
    }
}

#[test]
fn an_idle_claude_session_whose_box_namedrop_does_not_recognize_still_takes_the_mail() {
    // Herdr's idle is the proof, as before Namedrop read the box, so a screen
    // it can't read never strands the mail.
    for screen in [Some(MODEL_PICKER), Some("no prompt box drawn here\n"), None] {
        let fixture = Fixture::new(AgentRuntime::ClaudeCode, "idle");
        fixture.screen(screen);
        let (report, typed) = fixture.drain_report();
        assert_eq!(typed.len(), 1, "{screen:?}");
        assert!(report.draft_held.is_empty(), "{report:?}");
    }
}

#[test]
fn an_idle_codex_session_with_a_draft_gets_nothing_typed_into_it() {
    let fixture = Fixture::new(AgentRuntime::Codex, "idle");
    fixture.screen(Some(CODEX_DRAFT));
    let (held, typed) = fixture.drain_report();
    assert_eq!(typed, Vec::<String>::new());
    assert_eq!(held.draft_held.len(), 1, "{held:?}");
    fixture.screen(Some(CODEX_EMPTY));
    let (_, typed) = fixture.recheck(&held);
    assert_eq!(typed.len(), 1, "{typed:?}");
}

#[test]
fn draft_held_mail_goes_in_once_the_box_empties_with_no_stop_or_status_event() {
    for (status, edges) in [("idle", false), ("working", true)] {
        let fixture = Fixture::new(AgentRuntime::ClaudeCode, status);
        if edges {
            fixture.edge(TurnEdge::Prompt, 2_000);
            fixture.edge(TurnEdge::Stop, 3_000);
        }
        fixture.screen(Some(DRAFT_BOX));
        let (mut held, typed) = fixture.drain_report();
        assert_eq!(typed, Vec::<String>::new(), "{status}");
        assert_eq!(held.draft_held.len(), 1, "{status}: {held:?}");

        // While the draft stays, a recheck reads the box and nothing else.
        for _ in 0..3 {
            let (reads, lists) = (
                fixture.herdr.screen_reads.load(Ordering::Relaxed),
                fixture.herdr.pane_lists.load(Ordering::Relaxed),
            );
            let typed;
            (held, typed) = fixture.recheck(&held);
            assert_eq!(typed, Vec::<String>::new(), "{status}");
            assert_eq!(held.draft_held.len(), 1, "{status}: {held:?}");
            assert_eq!(
                fixture.herdr.screen_reads.load(Ordering::Relaxed),
                reads + 1
            );
            assert_eq!(fixture.herdr.pane_lists.load(Ordering::Relaxed), lists);
        }

        // The person clears the draft; Herdr's status does not change.
        fixture.screen(Some(EMPTY_BOX));
        let (delivered, typed) = fixture.recheck(&held);
        assert_eq!(typed.len(), 1, "{status}: {typed:?}");
        assert!(typed[0].contains("Are the tests green?"), "{typed:?}");
        assert!(delivered.draft_held.is_empty(), "{status}: {delivered:?}");
    }
}

/// The text of `DRAFT_BOX`'s box.
const DRAFT_TEXT: &str = "draft for scenario c, keep me";

impl Fixture {
    /// Namedrop's hook sent `prompt` as a message and blocked it at `at_ms`,
    /// and Claude Code put it back into the box.
    fn sent_mention(&self, at_ms: u64, prompt: &str) {
        self.edge(TurnEdge::Prompt, at_ms);
        self.edge(TurnEdge::Blocked, at_ms);
        record_restored(self.state(), "socket-a", SESSION, at_ms, prompt);
    }

    /// The keys each call pressed in the pane since the last call.
    fn keys(&self) -> Vec<Vec<String>> {
        std::mem::take(&mut *self.herdr.keys.lock().unwrap())
    }
}

/// One batch of the key that deletes a line of Claude Code's prompt box.
fn delete_batch() -> Vec<Vec<String>> {
    vec![vec!["ctrl+u".to_owned(); 32]]
}

fn no_keys() -> Vec<Vec<String>> {
    Vec::new()
}

/// Claude Code 2.1.322 puts a prompt a hook blocked back into the prompt box,
/// so a mention the person sent from the pane stays there. Namedrop deletes it
/// with the key that deletes a line of the box, then types the answer into the
/// empty box.
#[test]
fn a_box_holding_only_the_mention_namedrop_sent_is_cleared_and_takes_the_mail() {
    for (status, edges) in [("idle", false), ("working", true)] {
        let fixture = Fixture::new(AgentRuntime::ClaudeCode, status);
        if edges {
            fixture.edge(TurnEdge::Prompt, 2_000);
            fixture.edge(TurnEdge::Stop, 3_000);
        }
        // Claude Code wraps and indents the text it puts back.
        fixture.sent_mention(4_000, "draft for  scenario c,\nkeep me");
        fixture.screen(Some(DRAFT_BOX));
        let (report, typed) = fixture.drain_report();
        assert_eq!(fixture.keys(), delete_batch(), "{status}");
        assert_eq!(typed.len(), 1, "{status}: {typed:?}");
        assert!(typed[0].contains("Are the tests green?"), "{typed:?}");
        assert!(report.draft_held.is_empty(), "{status}: {report:?}");
    }

    // A box too short for the mention shows its last lines.
    let tall = include_str!("fixtures/between-turns/claude-2.1.322-tall-draft.ansi");
    let mention: String = (1..=80).map(|line| format!("tall line {line}\n")).collect();
    let fixture = Fixture::new(AgentRuntime::ClaudeCode, "idle");
    fixture.sent_mention(4_000, &mention);
    fixture.screen(Some(tall));
    assert_eq!(fixture.drain().len(), 1);
    assert_eq!(fixture.keys(), delete_batch());
}

/// Anything else in the box is the person's, and no key is sent: text they
/// added to the mention or typed in its place, and whatever the box holds
/// once a later prompt took the mention with it.
#[test]
fn a_box_that_holds_more_or_other_than_the_sent_mention_is_a_draft() {
    for sent in [
        // The person typed after the mention.
        "draft for scenario c",
        // The person typed in front of it.
        "keep me",
        "another mention, keep me",
        "",
    ] {
        let fixture = Fixture::new(AgentRuntime::ClaudeCode, "idle");
        fixture.sent_mention(4_000, sent);
        fixture.screen(Some(DRAFT_BOX));
        let (report, typed) = fixture.drain_report();
        assert_eq!((fixture.keys(), typed), (no_keys(), Vec::new()), "{sent:?}");
        assert_eq!(report.draft_held.len(), 1, "{sent:?}: {report:?}");
    }

    // The person sent a prompt after the mention, then typed the same words.
    let later = Fixture::new(AgentRuntime::ClaudeCode, "idle");
    later.sent_mention(4_000, DRAFT_TEXT);
    later.edge(TurnEdge::Prompt, 5_000);
    later.edge(TurnEdge::Stop, 6_000);
    later.screen(Some(DRAFT_BOX));
    assert_eq!((later.drain(), later.keys()), (Vec::new(), no_keys()));

    // Codex puts no prompt back, so nothing is deleted from its box.
    let codex = Fixture::new(AgentRuntime::Codex, "idle");
    codex.sent_mention(4_000, "my unsent draft");
    codex.screen(Some(CODEX_DRAFT));
    assert_eq!((codex.drain(), codex.keys()), (Vec::new(), no_keys()));
}

/// A draft held the mail, and the person then sent it as a mention. The
/// recheck finds the same words in the box, now put back by Claude Code.
#[test]
fn held_mail_goes_in_once_the_draft_was_sent_as_a_mention() {
    let fixture = Fixture::new(AgentRuntime::ClaudeCode, "idle");
    fixture.screen(Some(DRAFT_BOX));
    let (held, typed) = fixture.drain_report();
    assert_eq!((fixture.keys(), typed), (no_keys(), Vec::new()));
    assert_eq!(held.draft_held.len(), 1, "{held:?}");
    fixture.sent_mention(4_000, DRAFT_TEXT);
    let (delivered, typed) = fixture.recheck(&held);
    assert_eq!(fixture.keys(), delete_batch());
    assert_eq!(typed.len(), 1, "{typed:?}");
    assert!(delivered.draft_held.is_empty(), "{delivered:?}");
}

/// The keys leave the mention where it is, as in a session that takes them
/// another way. Nothing is typed behind it, one batch is pressed, and Doctor
/// says what is in the way.
#[test]
fn a_sent_mention_the_keys_leave_holds_the_mail_until_the_person_clears_it() {
    let fixture = Fixture::new(AgentRuntime::ClaudeCode, "idle");
    fixture.herdr.delete_empties.store(false, Ordering::Relaxed);
    fixture.sent_mention(4_000, DRAFT_TEXT);
    fixture.screen(Some(DRAFT_BOX));
    let (held, typed) = fixture.drain_report();
    assert_eq!((fixture.keys(), typed), (delete_batch(), Vec::new()));
    assert_eq!(held.draft_held.len(), 1, "{held:?}");
    // The worker's recheck reads the box and presses no key.
    let (held, typed) = fixture.recheck(&held);
    assert_eq!((fixture.keys(), typed), (no_keys(), Vec::new()));
    assert_eq!(held.draft_held.len(), 1, "{held:?}");
    let issue = fixture.long_wait().expect("Doctor explains the wait");
    assert_eq!(issue.diagnostic["held_by"], "sent_prompt", "{issue:?}");
    assert_eq!(fixture.keys(), no_keys());
    fixture.screen(Some(EMPTY_BOX));
    let (_, typed) = fixture.recheck(&held);
    assert_eq!(typed.len(), 1, "{typed:?}");
}

#[test]
fn the_recheck_stops_once_no_draft_holds_mail() {
    // The person sends the draft: its turn starts, so the mail waits on the
    // turn, which Herdr's events and the Stop cover.
    let sent = Fixture::new(AgentRuntime::ClaudeCode, "working");
    sent.edge(TurnEdge::Stop, 2_000);
    sent.screen(Some(DRAFT_BOX));
    let (held, _) = sent.drain_report();
    assert_eq!(held.draft_held.len(), 1);
    sent.edge(TurnEdge::Prompt, 3_000);
    sent.screen(Some(EMPTY_BOX));
    let (report, typed) = sent.recheck(&held);
    assert_eq!(typed, Vec::<String>::new());
    assert!(report.draft_held.is_empty(), "{report:?}");

    // The mailbox empties while the draft stays: the box is not read again.
    let cancelled = Fixture::new(AgentRuntime::ClaudeCode, "idle");
    cancelled.screen(Some(DRAFT_BOX));
    let (held, _) = cancelled.drain_report();
    assert_eq!(held.draft_held.len(), 1);
    StateStore::open(cancelled.state())
        .unwrap()
        .transaction(|transaction| {
            transaction.cancel_sender_delivery(
                &StateNamespace::for_space("socket-a", "workspace-1"),
                &PaneAddressId::new("address-author"),
                &DeliveryId::new("delivery-1"),
                &MutationAttribution::new(ActorKind::Human, "test", QUEUED_AT_MS),
            )
        })
        .unwrap();
    let reads = cancelled.herdr.screen_reads.load(Ordering::Relaxed);
    let (report, typed) = cancelled.recheck(&held);
    assert_eq!(typed, Vec::<String>::new());
    assert!(report.draft_held.is_empty(), "{report:?}");
    assert_eq!(cancelled.herdr.screen_reads.load(Ordering::Relaxed), reads);

    // Delivered mail leaves nothing to recheck.
    let delivered = Fixture::new(AgentRuntime::ClaudeCode, "idle");
    delivered.screen(Some(DRAFT_BOX));
    let (held, _) = delivered.drain_report();
    delivered.screen(Some(EMPTY_BOX));
    let (report, typed) = delivered.recheck(&held);
    assert_eq!(typed.len(), 1);
    assert!(report.draft_held.is_empty(), "{report:?}");
    delivered.set_status("working");
    let (report, _) = delivered.drain_report();
    assert!(report.draft_held.is_empty(), "{report:?}");
}
