//! Pins the Herdr arguments each port call runs, over a scripted transport.

use super::*;
use crate::{
    AgentRuntime, AgentSessionId, HerdrCommandOutput, SpaceId, WorktreeId,
    mux::{
        Admit, AgentIdentity, AgentKind, AgentState, Badge, Checkout, Gone, Key, MuxEvent,
        PaneFacts, PaneSlot, PopupScreen, PopupSpec, PromptRequest, Recipient, Retry, SessionClaim,
        StateTexts, Style, TerminalId, Wrap,
    },
};
use serde_json::json;
use std::{path::Path, sync::Mutex, time::Duration};

type Reply = Box<dyn Fn(&[&str]) -> Result<String, String> + Send + Sync>;

/// Records every command and answers from `reply`.
struct Scripted {
    calls: Mutex<Vec<String>>,
    reply: Reply,
}

impl Transport for Scripted {
    fn run(&self, args: &[&str]) -> Result<String, String> {
        self.calls.lock().unwrap().push(args.join(" "));
        (self.reply)(args)
    }
}

fn herdr(
    reply: impl Fn(&[&str]) -> Result<String, String> + Send + Sync + 'static,
) -> HerdrMux<Scripted> {
    HerdrMux::new(
        Scripted {
            calls: Mutex::new(Vec::new()),
            reply: Box::new(reply),
        },
        "socket-a",
    )
}

fn calls(mux: &HerdrMux<Scripted>) -> Vec<String> {
    mux.transport.calls.lock().unwrap().clone()
}

fn error(code: &str) -> String {
    json!({"error": {"code": code}}).to_string()
}

fn pane_list() -> String {
    json!({"result": {"panes": [
        {
            "pane_id": "p1", "terminal_id": "t1", "tab_id": "tab1", "workspace_id": "w1",
            "cwd": "/repo", "label": null, "title": "shell title",
            "terminal_title_stripped": "stripped", "agent": "claude",
            "display_agent": "Claude Code", "agent_session": {"value": "s1"},
            "agent_status": "idle", "state_labels": {"idle": "@author ready", "working": "w"},
        },
        {
            "pane_id": "p2", "terminal_id": "t2", "tab_id": "tab1", "workspace_id": "w2",
            "terminal_title_stripped": "zsh",
        },
        {
            "pane_id": "p3", "terminal_id": "t3", "tab_id": "tab2", "workspace_id": "w1",
            "label": "@reviewer", "agent_status": "thinking",
            "agent_session": {"value": "s3"},
        },
    ]}})
    .to_string()
}

fn worktrees(open: &str, path: &str) -> String {
    json!({"result": {
        "source": {"repo_key": "repo-key"},
        "worktrees": [
            {"path": "/elsewhere", "open_workspace_id": "w9"},
            {"path": path, "open_workspace_id": open, "label": "feature", "branch": "feat/x",
             "is_linked_worktree": true},
        ],
    }})
    .to_string()
}

fn space(id: &str) -> SpaceId {
    SpaceId::new(id)
}

#[test]
fn a_space_with_panes_is_one_worktree_listing_and_one_pane_listing() {
    let mux = herdr(|args| match args {
        ["worktree", "list", "--workspace", "w1", "--json"] => Ok(worktrees("w1", "/repo")),
        ["pane", "list", "--workspace", "w1"] => Ok(pane_list()),
        _ => Err("unexpected".into()),
    });
    let survey = mux
        .survey(Scope::Space(&space("w1")), Depth::Panes)
        .unwrap();
    assert_eq!(
        calls(&mux),
        [
            "worktree list --workspace w1 --json",
            "pane list --workspace w1"
        ]
    );
    let view = &survey.spaces[0];
    assert_eq!(view.worktree, Some(WorktreeId::new("/repo")));
    assert_eq!(
        view.checkout,
        Some(Checkout {
            path: "/repo".into(),
            label: Some("feature".into()),
            branch: Some("feat/x".into()),
            repo_key: Some("repo-key".into()),
            repo_name: None,
        })
    );
    assert_eq!(
        view.panes[0],
        PaneFacts {
            id: PaneId::new("p1"),
            terminal: TerminalId::new("t1"),
            tab: TabId::new("tab1"),
            space: space("w1"),
            cwd: Some("/repo".into()),
            title: Some("shell title".into()),
            agent: Some(AgentIdentity {
                kind: AgentKind::Known(AgentRuntime::ClaudeCode),
                label: Some("Claude Code".into()),
                session: SessionClaim::Reported(AgentSessionId::new("s1")),
            }),
            state: Some(AgentState::Idle),
            idle_label: Some("@author ready".into()),
        }
    );
    assert_eq!(view.panes[1].title.as_deref(), Some("zsh"));
    assert_eq!(view.panes[1].agent, None, "a free terminal pane");
    assert_eq!(
        view.panes[2].agent.as_ref().map(|agent| &agent.kind),
        Some(&AgentKind::Unreported)
    );
    assert_eq!(
        view.panes[2].state,
        Some(AgentState::Unknown("thinking".into()))
    );
}

#[test]
fn a_space_outside_git_keeps_the_synthetic_worktree_key() {
    let mux = herdr(|args| match args {
        ["worktree", ..] => Err(error("not_git_worktree")),
        ["pane", "list", "--workspace", "w1"] => Ok(json!({"result": {"panes": []}}).to_string()),
        _ => Err("unexpected".into()),
    });
    let survey = mux
        .survey(Scope::Space(&space("w1")), Depth::Panes)
        .unwrap();
    assert_eq!(survey.spaces[0].checkout, None);
    assert_eq!(
        survey.spaces[0].worktree,
        Some(WorktreeId::new("herdr-space:w1"))
    );
    assert_eq!(calls(&mux).len(), 2);
}

#[test]
fn a_closed_space_is_gone_without_listing_its_panes() {
    let mux = herdr(|_| Err(error("workspace_not_found")));
    assert_eq!(
        mux.survey(Scope::Space(&space("w1")), Depth::Panes),
        Err(MuxError::Gone(Gone::Space, error("workspace_not_found")))
    );
    assert_eq!(calls(&mux), ["worktree list --workspace w1 --json"]);
}

#[test]
fn a_listing_that_omits_the_space_is_a_protocol_error() {
    let mux = herdr(|_| Ok(worktrees("w2", "/repo")));
    assert!(matches!(
        mux.survey(Scope::Space(&space("w1")), Depth::Panes),
        Err(MuxError::Protocol(message)) if message.contains("does not contain space w1")
    ));
}

#[test]
fn a_space_summary_is_one_workspace_get() {
    let mux = herdr(|_| {
        Ok(
            json!({"result": {"workspace": {"label": "main", "number": 3, "worktree": {
            "checkout_path": "/repo", "repo_key": "k", "repo_name": "repo"}}}})
            .to_string(),
        )
    });
    let survey = mux
        .survey(Scope::Space(&space("w1")), Depth::Spaces)
        .unwrap();
    assert_eq!(calls(&mux), ["workspace get w1"]);
    let view = &survey.spaces[0];
    assert_eq!(
        (view.label.as_deref(), view.number),
        (Some("main"), Some(3))
    );
    assert_eq!(
        view.checkout.as_ref().and_then(|c| c.repo_name.as_deref()),
        Some("repo")
    );
    assert_eq!(view.worktree, None);
}

#[test]
fn every_space_is_one_workspace_list() {
    let mux = herdr(|_| {
        Ok(json!({"result": {"workspaces": [
            {"workspace_id": "w2", "label": null, "number": 2},
            {"workspace_id": "w1", "label": "main", "number": 1,
             "worktree": {"checkout_path": "/repo", "repo_name": "repo"}},
        ]}})
        .to_string())
    });
    let spaces = mux.survey(Scope::All, Depth::Spaces).unwrap().spaces;
    assert_eq!(calls(&mux), ["workspace list"]);
    assert_eq!(
        spaces
            .iter()
            .map(|view| (view.id.as_str(), view.number, view.checkout.is_some()))
            .collect::<Vec<_>>(),
        [("w2", Some(2), false), ("w1", Some(1), true)]
    );
}

#[test]
fn every_pane_is_one_pane_list_grouped_by_space() {
    let mux = herdr(|_| Ok(pane_list()));
    let spaces = mux.survey(Scope::All, Depth::Panes).unwrap().spaces;
    assert_eq!(calls(&mux), ["pane list"]);
    assert_eq!(
        spaces
            .iter()
            .map(|view| (
                view.id.as_str(),
                view.panes
                    .iter()
                    .map(|pane| pane.id.as_str())
                    .collect::<Vec<_>>()
            ))
            .collect::<Vec<_>>(),
        [("w1", vec!["p1", "p3"]), ("w2", vec!["p2"])]
    );
}

/// `w1` opens `/repo`. `w2` is stamped on another checkout, `w3` is in
/// another repository, `w4` closed while listing, and `w5` is outside Git.
fn checkout_world(args: &[&str]) -> Result<String, String> {
    match args {
        ["worktree", "list", "--cwd", "/repo", "--json"] => Ok(worktrees("w1", "/repo")),
        ["workspace", "list"] => Ok(json!({"result": {"workspaces": [
            {"workspace_id": "w1"},
            {"workspace_id": "w2", "worktree": {"checkout_path": "/other"}},
            {"workspace_id": "w3"},
            {"workspace_id": "w4"},
            {"workspace_id": "w5"},
        ]}})
        .to_string()),
        ["worktree", "list", "--workspace", "w3", "--json"] => Ok(json!({"result": {
            "source": {"repo_key": "another"},
            "worktrees": [{"path": "/repo", "open_workspace_id": "w3"}],
        }})
        .to_string()),
        ["worktree", "list", "--workspace", "w4", "--json"] => Err(error("workspace_not_found")),
        ["worktree", "list", "--workspace", "w5", "--json"] => Err(error("not_git_worktree")),
        ["pane", "list", "--workspace", "w1"] => Ok(pane_list()),
        _ => Err(format!("unexpected {args:?}")),
    }
}

#[test]
fn a_checkout_lookup_proves_one_owner_before_listing_its_panes() {
    let mux = herdr(checkout_world);
    let survey = mux
        .survey(Scope::CheckoutOf(Path::new("/repo")), Depth::Panes)
        .unwrap();
    assert_eq!(
        calls(&mux),
        [
            "worktree list --cwd /repo --json",
            "workspace list",
            "worktree list --workspace w3 --json",
            "worktree list --workspace w4 --json",
            "worktree list --workspace w5 --json",
            "pane list --workspace w1",
        ]
    );
    assert_eq!(survey.spaces[0].id, space("w1"));
    assert_eq!(survey.spaces[0].panes.len(), 3);

    let spaces_only = herdr(checkout_world);
    spaces_only
        .survey(Scope::CheckoutOf(Path::new("/repo")), Depth::Spaces)
        .unwrap();
    assert_eq!(calls(&spaces_only).len(), 5, "no pane listing");
}

#[test]
fn a_checkout_open_in_two_spaces_is_refused_without_listing_panes() {
    let mux = herdr(|args| match args {
        ["workspace", "list"] => Ok(json!({"result": {"workspaces": [
            {"workspace_id": "w1"},
            {"workspace_id": "w2", "worktree": {"checkout_path": "/repo"}},
        ]}})
        .to_string()),
        _ => checkout_world(args),
    });
    let refused = mux.survey(Scope::CheckoutOf(Path::new("/repo")), Depth::Panes);
    assert_eq!(
        refused,
        Err(MuxError::Unavailable(
            "Checkout /repo is open in more than one Herdr space (w1, w2), so Namedrop can't \
             tell which one this command is for. Pass --herdr-workspace <space-id> (or \
             --herdr-pane <pane-id>)."
                .into()
        ))
    );
    assert!(!calls(&mux).iter().any(|call| call.starts_with("pane list")));
}

#[test]
fn reads_run_one_command_each() {
    let mux = herdr(|args| match args {
        ["pane", "read", ..] => Ok("screen text".into()),
        ["pane", "process-info", ..] => Ok(json!({"result": {"process_info": {
            "shell_pid": 10, "foreground_process_group_id": 11,
            "foreground_processes": [{"pid": 11}, {"pid": 12}], "tty": "/dev/ttys001"}}})
        .to_string()),
        ["tab", "get", "tab1"] => Ok(json!({"result": {"tab": {"label": "@author"}}}).to_string()),
        ["plugin", ..] => Ok(json!({"result": {"plugins": [
            {"plugin_id": "other", "plugin_root": "/x"},
            {"plugin_id": "namedrop", "plugin_root": "/plugins/namedrop"},
        ]}})
        .to_string()),
        _ => Err("unexpected".into()),
    });
    let pane = PaneId::new("p1");
    for (wrap, style) in [(Wrap::AsDrawn, Style::Ansi), (Wrap::Joined, Style::Plain)] {
        let request = ScreenRequest {
            lines: 60,
            wrap,
            style,
        };
        assert_eq!(mux.screen(&pane, request).unwrap().text, "screen text");
    }
    assert_eq!(
        mux.process(&pane),
        Ok(PaneProcess {
            shell_pid: Some(10),
            foreground_group: Some(11),
            foreground_pids: vec![11, 12],
            tty: Some("/dev/ttys001".into()),
        })
    );
    assert_eq!(
        mux.tab_label(&TabId::new("tab1")),
        Ok(Some("@author".into()))
    );
    assert_eq!(mux.integration_root(), Ok(Some("/plugins/namedrop".into())));
    assert_eq!(
        calls(&mux),
        [
            "pane read p1 --source recent --lines 60 --format ansi",
            "pane read p1 --source recent-unwrapped --lines 60",
            "pane process-info --pane p1",
            "tab get tab1",
            "plugin list --plugin namedrop --json",
        ]
    );
}

#[test]
fn transport_failures_map_to_port_errors() {
    for (failure, expected) in [
        (
            error("pane_not_found"),
            MuxError::Gone(Gone::Pane, error("pane_not_found")),
        ),
        (
            "Herdr command timed out after 10000 ms".to_owned(),
            MuxError::Timeout("Herdr command timed out after 10000 ms".to_owned()),
        ),
        (
            "Herdr read exceeded its output limit".to_owned(),
            MuxError::TooLarge("Herdr read exceeded its output limit".to_owned()),
        ),
        (
            error("server_busy"),
            MuxError::Unavailable(error("server_busy")),
        ),
    ] {
        let reply = failure.clone();
        let mux = herdr(move |_| Err(reply.clone()));
        assert_eq!(mux.process(&PaneId::new("p1")), Err(expected), "{failure}");
    }
    let garbled = herdr(|_| Ok("not json".into()));
    assert!(matches!(
        garbled.process(&PaneId::new("p1")),
        Err(MuxError::Protocol(_))
    ));
}

fn target() -> PaneFacts {
    PaneFacts {
        id: PaneId::new("p1"),
        terminal: TerminalId::new("t1"),
        tab: TabId::new("tab1"),
        space: space("w1"),
        cwd: None,
        title: None,
        agent: Some(AgentIdentity {
            kind: AgentKind::Known(AgentRuntime::ClaudeCode),
            label: None,
            session: SessionClaim::Reported(AgentSessionId::new("s1")),
        }),
        state: None,
        idle_label: None,
    }
}

#[test]
fn each_effect_runs_today_s_command() {
    let mux = herdr(|_| Ok("{}".into()));
    let texts = StateTexts {
        ready: "@a ready".into(),
        working: "@a working".into(),
        attention: "@a needs attention".into(),
        unknown: "@a status unknown".into(),
    };
    for effect in [
        Effect::Identify {
            pane: PaneId::new("p1"),
            alias: "a".into(),
            texts,
        },
        Effect::Badges {
            pane: PaneId::new("p1"),
            badges: vec![
                Badge::Awaiting(2),
                Badge::Attention(0),
                Badge::Relay {
                    alias: "b".into(),
                    summary: "done".into(),
                },
            ],
        },
        Effect::RenameTab {
            tab: TabId::new("tab1"),
            label: "@a".into(),
        },
        Effect::Notify {
            title: "@b replied".into(),
            body: "done".into(),
            sound: true,
        },
        Effect::Notify {
            title: "Namedrop".into(),
            body: "Sent to @b".into(),
            sound: false,
        },
        Effect::OpenPopup(PopupSpec {
            screen: PopupScreen::Status,
            target: target(),
            state_dir: "/state".into(),
        }),
        Effect::OpenPopup(PopupSpec {
            screen: PopupScreen::Tour,
            target: target(),
            state_dir: "/state".into(),
        }),
        Effect::ReloadKeybindings,
    ] {
        assert_eq!(mux.apply(&effect), Ok(Applied::Done));
    }
    let popup_env = "--env NAMEDROP_TARGET_WORKSPACE=w1 --env NAMEDROP_TARGET_PANE=p1 \
                     --env NAMEDROP_TARGET_TERMINAL=t1 --env NAMEDROP_TARGET_SESSION=s1 \
                     --env HERDR_PLUGIN_STATE_DIR=/state --env HERDR_SOCKET_PATH=socket-a --focus";
    assert_eq!(
        calls(&mux),
        [
            "pane report-metadata p1 --source namedrop-identity --title @a \
             --state-label idle=@a ready --state-label done=@a ready \
             --state-label working=@a working --state-label blocked=@a needs attention \
             --state-label unknown=@a status unknown"
                .to_owned(),
            "pane report-metadata p1 --source namedrop --token namedrop-awaiting=2 \
             --token namedrop-attention=0"
                .to_owned(),
            "pane report-metadata p1 --source namedrop-relay --token namedrop-relay=@b: done \
             --ttl-ms 15000"
                .to_owned(),
            "tab rename tab1 @a".to_owned(),
            "notification show @b replied --body done --position bottom-right --sound done"
                .to_owned(),
            "notification show Namedrop --body Sent to @b --position bottom-right".to_owned(),
            format!(
                "plugin pane open --plugin namedrop --entrypoint agent-status --placement popup \
                 --width 84 --height 24 {popup_env}"
            ),
            format!(
                "plugin pane open --plugin namedrop --entrypoint tour --placement popup \
                 --width 72 --height 24 {popup_env}"
            ),
            "server reload-config".to_owned(),
        ]
    );
}

#[test]
fn the_tour_popup_matches_the_tour_module_s_own_arguments() {
    let mux = herdr(|_| Ok("{}".into()));
    mux.apply(&Effect::OpenPopup(PopupSpec {
        screen: PopupScreen::Tour,
        target: target(),
        state_dir: "/state".into(),
    }))
    .unwrap();
    let expected =
        crate::tour::popup_open_args("w1", "p1", "t1", "s1", Path::new("/state"), "socket-a");
    assert_eq!(calls(&mux), [expected.join(" ")]);
}

#[test]
fn input_runs_today_s_command_for_each_kind() {
    let mux = herdr(|_| Ok("{}".into()));
    let pane = PaneId::new("p1");
    let expect = target().agent.unwrap();
    let request = PromptRequest {
        expect: &expect,
        text: "hello there",
        admit: Admit::BetweenTurns,
    };
    for kind in [
        InputKind::Prompt(request),
        InputKind::Line("namedrop terminal-execute"),
        InputKind::Keys {
            recipient: Recipient::Agent,
            keys: &[Key::CtrlU, Key::CtrlU],
        },
        InputKind::Keys {
            recipient: Recipient::Pane,
            keys: &[Key::Escape, Key::Enter, Key::Up, Key::Down, Key::CtrlC],
        },
    ] {
        assert_eq!(mux.send(&pane, kind), InputOutcome::Delivered);
    }
    assert_eq!(
        calls(&mux),
        [
            "agent prompt p1 hello there",
            "pane run p1 namedrop terminal-execute",
            "agent send-keys p1 ctrl+u ctrl+u",
            "pane send-keys p1 esc enter up down ctrl+c",
        ]
    );
}

#[test]
fn herdr_error_codes_decide_the_input_outcome() {
    for (code, outcome) in [
        (
            "agent_not_found",
            InputOutcome::Declined(Retry::OnEvent, error("agent_not_found")),
        ),
        (
            "pane_not_found",
            InputOutcome::Declined(Retry::OnEvent, error("pane_not_found")),
        ),
        (
            "agent_busy",
            InputOutcome::Declined(Retry::OnEvent, error("agent_busy")),
        ),
        (
            "agent_blocked",
            InputOutcome::Declined(Retry::OnEvent, error("agent_blocked")),
        ),
        (
            "agent_not_ready",
            InputOutcome::Declined(Retry::AfterDelay, error("agent_not_ready")),
        ),
        (
            "conflict",
            InputOutcome::Declined(Retry::AfterDelay, error("conflict")),
        ),
        (
            "server_busy",
            InputOutcome::Declined(Retry::AfterDelay, error("server_busy")),
        ),
        (
            "invalid_request",
            InputOutcome::Refused(error("invalid_request")),
        ),
        ("invalid_args", InputOutcome::Refused(error("invalid_args"))),
        (
            "invalid_input",
            InputOutcome::Refused(error("invalid_input")),
        ),
        ("unsupported", InputOutcome::Refused(error("unsupported"))),
        (
            "unknown_command",
            InputOutcome::Refused(error("unknown_command")),
        ),
        ("internal", InputOutcome::Ambiguous(error("internal"))),
    ] {
        let reply = error(code);
        let mux = herdr(move |_| Err(reply.clone()));
        assert_eq!(
            mux.send(&PaneId::new("p1"), InputKind::Line("ls")),
            outcome,
            "{code}"
        );
    }
    let timed_out = herdr(|_| Err("Herdr command timed out after 10000 ms".into()));
    assert!(matches!(
        timed_out.send(&PaneId::new("p1"), InputKind::Line("ls")),
        InputOutcome::Ambiguous(_)
    ));
}

/// A legacy executor with its own classification, as several test fakes have.
struct Legacy {
    prompts: Mutex<Vec<String>>,
}

impl HerdrCommandExecutor for Legacy {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        match args {
            ["pane", "list"] => Ok(HerdrCommandOutput {
                stdout: pane_list(),
            }),
            ["agent", "prompt", _, text] => {
                self.prompts.lock().unwrap().push((*text).to_owned());
                Err(error("agent_busy"))
            }
            _ => Err("unexpected".into()),
        }
    }

    fn input_failure(&self, error: String) -> InputOutcome {
        InputOutcome::Refused(error)
    }
}

#[test]
fn every_legacy_executor_is_a_mux_that_keeps_its_own_classification() {
    let legacy = Legacy {
        prompts: Mutex::new(Vec::new()),
    };
    let spaces = MuxRead::survey(&legacy, Scope::All, Depth::Panes)
        .unwrap()
        .spaces;
    assert_eq!(spaces.len(), 2);
    assert_eq!(MuxRead::endpoint(&legacy).kind, MuxKind::Herdr);
    let expect = target().agent.unwrap();
    let request = PromptRequest {
        expect: &expect,
        text: "hello",
        admit: Admit::Settled,
    };
    assert_eq!(
        bridge(&legacy).send(&PaneId::new("p1"), InputKind::Prompt(request)),
        InputOutcome::Refused(error("agent_busy"))
    );
    assert_eq!(*legacy.prompts.lock().unwrap(), ["hello"]);
}

/// Records the budget each bounded read was handed.
struct Budgeted {
    reads: Mutex<Vec<(String, Duration, usize, bool)>>,
}

impl HerdrCommandExecutor for Budgeted {
    fn execute(&self, _args: &[&str]) -> Result<HerdrCommandOutput, String> {
        panic!("a bounded read must not run unbounded")
    }

    fn execute_bounded(
        &self,
        args: &[&str],
        timeout: Duration,
        cancelled: &AtomicBool,
        max_output_bytes: usize,
    ) -> Result<HerdrCommandOutput, String> {
        let cancelled = cancelled.load(std::sync::atomic::Ordering::Relaxed);
        self.reads
            .lock()
            .unwrap()
            .push((args.join(" "), timeout, max_output_bytes, cancelled));
        Ok(HerdrCommandOutput {
            stdout: json!({"result": {"label": "build"}}).to_string(),
        })
    }
}

#[test]
fn a_bounded_read_asks_its_budget_before_every_command() {
    let executor = Budgeted {
        reads: Mutex::new(Vec::new()),
    };
    let cancelled = AtomicBool::new(true);
    let left = Mutex::new(vec![Duration::from_millis(700), Duration::from_millis(900)]);
    let reads = bounded(&executor, &cancelled, || {
        let timeout = left.lock().unwrap().pop().ok_or("the budget ran out")?;
        Ok(Budget {
            timeout,
            max_output_bytes: 4096,
        })
    });
    let tab = TabId::new("tab1");
    assert_eq!(reads.tab_label(&tab).unwrap().as_deref(), Some("build"));
    assert_eq!(reads.tab_label(&tab).unwrap().as_deref(), Some("build"));
    // The budget's own words reach the caller, and no command runs.
    assert_eq!(
        reads.tab_label(&tab).unwrap_err(),
        MuxError::Unavailable("the budget ran out".to_owned())
    );
    let read = |timeout| {
        (
            "tab get tab1".to_owned(),
            Duration::from_millis(timeout),
            4096,
            true,
        )
    };
    assert_eq!(*executor.reads.lock().unwrap(), [read(900), read(700)]);
}

fn pane_event(pane: &str, workspace: &str, tab: &str) -> serde_json::Value {
    json!({
        "pane_id": pane, "workspace_id": workspace, "terminal_id": "t1", "tab_id": tab,
        "focused": true, "agent_status": "idle", "revision": 1,
    })
}

fn slot(space: &str, pane: &str, tab: Option<&str>) -> PaneSlot {
    PaneSlot {
        space: SpaceId::new(space),
        pane: PaneId::new(pane),
        tab: tab.map(TabId::new),
    }
}

#[test]
fn plugin_events_decode_to_id_only_mux_events() {
    let lifecycle = "event.pane-lifecycle";
    let status = "event.agent-status-changed";
    for (hook, operation, data, expected) in [
        (
            "pane.created",
            lifecycle,
            json!({"type": "pane_created", "pane": pane_event("p1", "w1", "tab1")}),
            MuxEvent::PaneAppeared(slot("w1", "p1", Some("tab1"))),
        ),
        (
            "pane.closed",
            lifecycle,
            json!({"type": "pane_closed", "pane_id": "p1", "workspace_id": "w1"}),
            MuxEvent::PaneClosed(slot("w1", "p1", None)),
        ),
        (
            "pane.exited",
            lifecycle,
            json!({"type": "pane_exited", "pane_id": "p1", "workspace_id": "w1"}),
            MuxEvent::PaneClosed(slot("w1", "p1", None)),
        ),
        (
            "tab.closed",
            lifecycle,
            json!({"type": "tab_closed", "tab_id": "tab1", "workspace_id": "w1"}),
            MuxEvent::TabClosed {
                space: SpaceId::new("w1"),
                tab: TabId::new("tab1"),
            },
        ),
        (
            "pane.moved",
            lifecycle,
            json!({"type": "pane_moved", "previous_pane_id": "p1",
                   "previous_workspace_id": "w1", "previous_tab_id": "tab1",
                   "pane": pane_event("p2", "w2", "tab2"), "closed_tab_id": "tab1"}),
            MuxEvent::PaneMoved {
                from: slot("w1", "p1", Some("tab1")),
                to: slot("w2", "p2", Some("tab2")),
            },
        ),
        (
            "pane.agent_status_changed",
            status,
            json!({"type": "pane_agent_status_changed", "pane_id": "p1",
                   "workspace_id": "w1", "agent_status": "working"}),
            MuxEvent::AgentStatusChanged(slot("w1", "p1", None)),
        ),
        (
            "pane.agent_detected",
            status,
            json!({"type": "pane_agent_detected", "pane_id": "p1", "workspace_id": "w1",
                   "agent": "codex", "released": false}),
            MuxEvent::AgentDetected(slot("w1", "p1", None)),
        ),
    ] {
        let event = data["type"].as_str().unwrap().to_owned();
        let envelope = json!({"event": event, "data": data}).to_string();
        assert_eq!(
            decode_plugin_event(operation, hook, &envelope),
            Ok(expected),
            "{hook}"
        );
    }
}

#[test]
fn a_plugin_event_must_match_its_hook_and_operation_and_schema() {
    let data = json!({"type": "pane_closed", "pane_id": "p1", "workspace_id": "w1"});
    let envelope = json!({"event": "pane_closed", "data": data}).to_string();
    assert!(decode_plugin_event("event.pane-lifecycle", "pane.created", &envelope).is_err());
    assert!(decode_plugin_event("event.agent-status-changed", "pane.closed", &envelope).is_err());
    let unknown = json!({"event": "pane_resized", "data": {}}).to_string();
    assert_eq!(
        decode_plugin_event("event.pane-lifecycle", "pane.resized", &unknown),
        Err("unsupported Herdr plugin event: pane_resized".into())
    );
    let extra = json!({"type": "pane_closed", "pane_id": "p1", "workspace_id": "w1", "new": 1});
    assert!(decode_operation_input("event.pane-lifecycle", extra).is_err());
}
