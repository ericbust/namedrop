//! The same behavioral tests against `FakeMux` and against `HerdrMux` over a
//! transport that answers Herdr commands from a fake world, so the fake
//! cannot drift from what the adapter reads and does.

use super::{HerdrMux, transport::Transport};
use crate::{
    AgentRuntime, SpaceId,
    mux::{
        Admit, AgentIdentity, AgentKind, AgentState, Badge, Depth, Effect, Gone, InputKind,
        InputOutcome, Key, MuxAct, MuxError, MuxRead, PaneId, PaneProcess, PromptRequest,
        Recipient, Retry, Scope, Screen, ScreenRequest, SessionClaim, StateTexts, Style, Survey,
        TabId, Wrap,
        fake::{FakeMux, FakePane, Sent, SentInput, World, WorldBuilder},
    },
};
use serde_json::{Value, json};
use std::{fmt::Debug, path::Path};

/// Answers Herdr commands the way Herdr would for the fake's world.
struct Wire(FakeMux);

fn herdr_error(code: &str) -> String {
    json!({"error": {"code": code}}).to_string()
}

fn agent_word(kind: &AgentKind) -> Option<String> {
    match kind {
        AgentKind::Known(AgentRuntime::ClaudeCode) => Some("claude".into()),
        AgentKind::Known(AgentRuntime::Codex) => Some("codex".into()),
        AgentKind::Other(name) => Some(name.clone()),
        AgentKind::Unreported => None,
    }
}

fn pane_json(pane: &FakePane) -> Value {
    let facts = &pane.facts;
    let agent = facts.agent.as_ref();
    let word = agent.and_then(|agent| agent_word(&agent.kind));
    let display = agent
        .and_then(|agent| agent.label.clone())
        .filter(|label| Some(label) != word.as_ref());
    let session = agent.and_then(|agent| match &agent.session {
        SessionClaim::Reported(session) => Some(json!({"value": session.as_str()})),
        _ => None,
    });
    json!({
        "pane_id": facts.id.as_str(),
        "terminal_id": facts.terminal.as_str(),
        "tab_id": facts.tab.as_str(),
        "workspace_id": facts.space.as_str(),
        "cwd": facts.cwd,
        "label": facts.title,
        "agent": word,
        "display_agent": display,
        "agent_session": session,
        "agent_status": facts.state.as_ref().map(AgentState::as_str),
        "state_labels": facts.idle_label.as_ref().map(|idle| json!({"idle": idle})),
    })
}

fn key(word: &str) -> Key {
    match word {
        "enter" => Key::Enter,
        "esc" => Key::Escape,
        "ctrl+c" => Key::CtrlC,
        "ctrl+u" => Key::CtrlU,
        "up" => Key::Up,
        "down" => Key::Down,
        other => panic!("unknown key {other}"),
    }
}

impl Wire {
    fn space<'w>(world: &'w World, id: &str) -> Result<&'w crate::mux::fake::FakeSpace, String> {
        world
            .spaces
            .iter()
            .find(|space| space.id.as_str() == id)
            .ok_or_else(|| herdr_error("workspace_not_found"))
    }

    fn pane<'w>(world: &'w World, id: &str) -> Result<&'w FakePane, String> {
        world
            .pane(&PaneId::new(id))
            .ok_or_else(|| herdr_error("pane_not_found"))
    }

    fn answer(&self, args: &[&str]) -> Result<Value, String> {
        let mut world = self.0.lock();
        Ok(match args {
            ["worktree", "list", "--workspace", id, "--json"] => {
                let space = Self::space(&world, id)?;
                let checkout = space
                    .checkout
                    .as_ref()
                    .ok_or_else(|| herdr_error("not_git_worktree"))?;
                json!({"result": {
                    "source": {"repo_key": checkout.repo_key},
                    "worktrees": [{
                        "path": checkout.path, "open_workspace_id": id,
                        "label": checkout.label, "branch": checkout.branch,
                        "is_linked_worktree": true,
                    }],
                }})
            }
            ["worktree", "list", "--cwd", cwd, "--json"] => {
                let opener = world.spaces.iter().find(|space| {
                    space
                        .checkout
                        .as_ref()
                        .is_some_and(|checkout| checkout.path == Path::new(cwd))
                });
                match opener.and_then(|space| Some((space, space.checkout.as_ref()?))) {
                    Some((space, checkout)) => json!({"result": {
                        "source": {"repo_key": checkout.repo_key},
                        "worktrees": [{
                            "path": cwd, "open_workspace_id": space.id.as_str(),
                            "label": checkout.label, "branch": checkout.branch,
                            "is_linked_worktree": true,
                        }],
                    }}),
                    None => json!({"result": {"source": null, "worktrees": []}}),
                }
            }
            ["workspace", "list"] => {
                let workspaces: Vec<_> = world
                    .spaces
                    .iter()
                    .map(|space| {
                        json!({
                            "workspace_id": space.id.as_str(),
                            "label": space.label,
                            "number": space.number,
                            "worktree": space.checkout.as_ref().map(|checkout| json!({
                                "checkout_path": checkout.path,
                                "repo_key": checkout.repo_key,
                                "repo_name": checkout.repo_name,
                            })),
                        })
                    })
                    .collect();
                json!({"result": {"workspaces": workspaces}})
            }
            ["workspace", "get", id] => {
                let space = Self::space(&world, id)?;
                json!({"result": {"workspace": {
                    "label": space.label,
                    "number": space.number,
                    "worktree": space.checkout.as_ref().map(|checkout| json!({
                        "checkout_path": checkout.path,
                        "repo_key": checkout.repo_key,
                        "repo_name": checkout.repo_name,
                    })),
                }}})
            }
            ["pane", "list"] => {
                let panes: Vec<_> = world
                    .spaces
                    .iter()
                    .flat_map(|space| &space.panes)
                    .map(pane_json)
                    .collect();
                json!({"result": {"panes": panes}})
            }
            ["pane", "list", "--workspace", id] => {
                let panes: Vec<_> = Self::space(&world, id)?
                    .panes
                    .iter()
                    .map(pane_json)
                    .collect();
                json!({"result": {"panes": panes}})
            }
            ["pane", "read", id, "--source", _, "--lines", lines, ..] => {
                let screen = &Self::pane(&world, id)?.screen;
                let lines: usize = lines.parse().unwrap();
                let all: Vec<_> = screen.lines().collect();
                return Ok(Value::String(
                    all[all.len().saturating_sub(lines)..].join("\n"),
                ));
            }
            ["pane", "process-info", "--pane", id] => {
                let process = &Self::pane(&world, id)?.process;
                let foreground: Vec<_> = process
                    .foreground_pids
                    .iter()
                    .map(|pid| json!({"pid": pid}))
                    .collect();
                json!({"result": {"process_info": {
                    "shell_pid": process.shell_pid,
                    "foreground_process_group_id": process.foreground_group,
                    "foreground_processes": foreground,
                    "tty": process.tty,
                }}})
            }
            ["tab", "get", tab] => match world.tab_labels.get(&TabId::new(*tab)) {
                Some(label) => json!({"result": {"label": label}}),
                None => json!({"result": {}}),
            },
            ["tab", "rename", tab, label] => {
                world
                    .tab_labels
                    .insert(TabId::new(*tab), (*label).to_owned());
                json!({})
            }
            ["plugin", "list", "--plugin", "namedrop", "--json"] => {
                let plugins: Vec<_> = world
                    .integration_root
                    .iter()
                    .map(|root| json!({"plugin_id": "namedrop", "plugin_root": root}))
                    .collect();
                json!({"result": {"plugins": plugins}})
            }
            ["agent", "prompt", id, text] => {
                let facts = &Self::pane(&world, id)?.facts;
                if facts.agent.is_none() {
                    return Err(herdr_error("agent_not_found"));
                }
                match facts.state {
                    Some(AgentState::Blocked) => return Err(herdr_error("agent_blocked")),
                    Some(ref state) if state.accepts_prompt() => {}
                    _ => return Err(herdr_error("agent_busy")),
                }
                let pane = PaneId::new(*id);
                world.record_sent(Sent {
                    pane: pane.clone(),
                    input: SentInput::Prompt((*text).to_owned()),
                });
                world.set_state(&pane, AgentState::Working);
                json!({})
            }
            ["pane", "run", id, line] => {
                Self::pane(&world, id)?;
                world.record_sent(Sent {
                    pane: PaneId::new(*id),
                    input: SentInput::Line((*line).to_owned()),
                });
                json!({})
            }
            [owner @ ("agent" | "pane"), "send-keys", id, keys @ ..] => {
                Self::pane(&world, id)?;
                let recipient = if *owner == "agent" {
                    Recipient::Agent
                } else {
                    Recipient::Pane
                };
                world.record_sent(Sent {
                    pane: PaneId::new(*id),
                    input: SentInput::Keys {
                        recipient,
                        keys: keys.iter().map(|word| key(word)).collect(),
                    },
                });
                json!({})
            }
            ["pane", "report-metadata", id, rest @ ..] => {
                let pane = world
                    .pane_mut(&PaneId::new(*id))
                    .ok_or_else(|| herdr_error("pane_not_found"))?;
                for pair in rest.windows(2) {
                    match pair {
                        ["--title", title] => pane.facts.title = Some((*title).to_owned()),
                        ["--state-label", label] => {
                            if let Some(idle) = label.strip_prefix("idle=") {
                                pane.facts.idle_label = Some(idle.to_owned());
                            }
                        }
                        _ => {}
                    }
                }
                json!({})
            }
            ["notification", "show", ..] | ["plugin", "pane", "open", ..] => json!({}),
            ["server", "reload-config"] => json!({}),
            other => panic!("the conformance wire does not answer {other:?}"),
        })
    }
}

impl Transport for Wire {
    fn run(&self, args: &[&str]) -> Result<String, String> {
        self.answer(args).map(|value| match value {
            Value::String(text) => text,
            json => json.to_string(),
        })
    }
}

trait Subject: MuxRead + MuxAct {
    fn send(&self, pane: &PaneId, kind: InputKind<'_>) -> InputOutcome;
}

impl Subject for FakeMux {
    fn send(&self, pane: &PaneId, kind: InputKind<'_>) -> InputOutcome {
        FakeMux::send(self, pane, kind)
    }
}

impl Subject for HerdrMux<Wire> {
    fn send(&self, pane: &PaneId, kind: InputKind<'_>) -> InputOutcome {
        HerdrMux::send(self, pane, kind)
    }
}

/// Runs `check` against a fresh `FakeMux` and a fresh `HerdrMux` over the
/// same world, asserts they agree, and returns the answer.
fn agree<R: Debug + PartialEq>(
    world: fn() -> WorldBuilder,
    check: impl Fn(&dyn Subject, &FakeMux) -> R,
) -> R {
    let fake = world().build();
    let from_fake = check(&fake, &fake);
    let herdr_world = world().build();
    let herdr = HerdrMux::new(Wire(herdr_world.clone()), "fake-socket");
    let from_herdr = check(&herdr, &herdr_world);
    assert_eq!(from_fake, from_herdr, "FakeMux and HerdrMux disagree");
    from_fake
}

fn world() -> WorldBuilder {
    FakeMux::world()
        .space("workspace-1")
        .label("main")
        .checkout("/repo")
        .branch("main")
        .repo("github.com/acme/repo", "repo")
        .pane("pane-author")
        .claude()
        .session("session-author")
        .idle()
        .idle_label("@author ready")
        .title("@author")
        .cwd("/repo")
        .screen("one\ntwo\nthree")
        .process(PaneProcess {
            shell_pid: Some(41),
            foreground_group: Some(42),
            foreground_pids: vec![42, 43],
            tty: Some("/dev/ttys004".into()),
        })
        .pane("pane-reviewer")
        .codex()
        .session("session-reviewer")
        .working()
        .tab("workspace-1:tab-2")
        .pane("pane-blocked")
        .other_agent("aider")
        .blocked()
        .space("workspace-2")
        .label("scratch")
        .pane("pane-shell")
        .pane("pane-orphan")
        .session("session-orphan")
        .space("workspace-3")
        .checkout("/other")
        .tab_label("workspace-1:tab-1", "@author")
        .integration_root("/plugins/namedrop")
}

fn survey(mux: &dyn Subject, scope: Scope<'_>, depth: Depth) -> Result<Survey, MuxError> {
    mux.survey(scope, depth)
}

#[test]
fn every_scope_and_depth_reports_the_same_survey() {
    let one = SpaceId::new("workspace-1");
    let two = SpaceId::new("workspace-2");
    for depth in [Depth::Spaces, Depth::Panes] {
        for scope in [
            Scope::Space(&one),
            Scope::Space(&two),
            Scope::CheckoutOf(Path::new("/repo")),
            Scope::All,
        ] {
            let found = agree(world, |mux, _| survey(mux, scope, depth));
            assert!(found.is_ok(), "{scope:?} {depth:?}: {found:?}");
        }
    }
}

#[test]
fn a_survey_carries_the_agent_identity_status_and_idle_label() {
    let first = agree(world, |mux, _| {
        survey(
            mux,
            Scope::Space(&SpaceId::new("workspace-1")),
            Depth::Panes,
        )
    })
    .unwrap();
    let author = &first.spaces[0].panes[0];
    assert_eq!(
        author.agent.as_ref().map(|agent| &agent.kind),
        Some(&AgentKind::Known(AgentRuntime::ClaudeCode))
    );
    assert_eq!(author.idle_label.as_deref(), Some("@author ready"));
    let orphan = agree(world, |mux, _| {
        survey(
            mux,
            Scope::Space(&SpaceId::new("workspace-2")),
            Depth::Panes,
        )
    })
    .unwrap();
    assert_eq!(orphan.spaces[0].panes[0].agent, None);
    assert_eq!(
        orphan.spaces[0].panes[1]
            .agent
            .as_ref()
            .map(|agent| &agent.kind),
        Some(&AgentKind::Unreported)
    );
}

#[test]
fn a_missing_space_is_gone_and_an_unheld_checkout_is_unavailable() {
    let missing = SpaceId::new("workspace-9");
    assert_eq!(
        agree(world, |mux, _| survey(
            mux,
            Scope::Space(&missing),
            Depth::Panes
        )),
        Err(MuxError::Gone(Gone::Space))
    );
    let unheld = agree(world, |mux, _| {
        matches!(
            survey(mux, Scope::CheckoutOf(Path::new("/nowhere")), Depth::Panes),
            Err(MuxError::Unavailable(_))
        )
    });
    assert!(unheld);
}

#[test]
fn reads_of_screens_processes_tabs_and_the_integration_root_agree() {
    let read = |mux: &dyn Subject, _: &FakeMux| {
        let request = ScreenRequest {
            lines: 2,
            wrap: Wrap::AsDrawn,
            style: Style::Ansi,
        };
        (
            mux.screen(&PaneId::new("pane-author"), request),
            mux.process(&PaneId::new("pane-author")),
            mux.process(&PaneId::new("pane-gone")),
            mux.tab_label(&TabId::new("workspace-1:tab-1")),
            mux.tab_label(&TabId::new("workspace-1:tab-2")),
            mux.integration_root(),
        )
    };
    let (screen, _, gone, label, unlabeled, root) = agree(world, read);
    assert_eq!(
        screen,
        Ok(Screen {
            text: "two\nthree".into()
        })
    );
    assert_eq!(gone, Err(MuxError::Gone(Gone::Pane)));
    assert_eq!(label, Ok(Some("@author".into())));
    assert_eq!(unlabeled, Ok(None));
    assert_eq!(root, Ok(Some("/plugins/namedrop".into())));
}

fn prompt(mux: &dyn Subject, pane: &str) -> InputOutcome {
    let expect = AgentIdentity {
        kind: AgentKind::Known(AgentRuntime::ClaudeCode),
        label: None,
        session: SessionClaim::None,
    };
    let request = PromptRequest {
        expect: &expect,
        text: "hello",
        admit: Admit::Settled,
    };
    mux.send(&PaneId::new(pane), InputKind::Prompt(request))
}

#[test]
fn a_settled_agent_takes_a_prompt_and_starts_working() {
    let (outcome, sent, state) = agree(world, |mux, world| {
        let outcome = prompt(mux, "pane-author");
        let state = mux
            .survey(Scope::Space(&SpaceId::new("workspace-1")), Depth::Panes)
            .map(|survey| survey.spaces[0].panes[0].state.clone());
        (outcome, world.sent(), state)
    });
    assert_eq!(outcome, InputOutcome::Delivered);
    assert_eq!(
        sent,
        [Sent {
            pane: PaneId::new("pane-author"),
            input: SentInput::Prompt("hello".into()),
        }]
    );
    assert_eq!(state, Ok(Some(AgentState::Working)));
}

#[test]
fn a_busy_blocked_absent_or_missing_agent_is_declined_before_any_input() {
    for pane in ["pane-reviewer", "pane-blocked", "pane-shell", "pane-gone"] {
        let (outcome, sent) = agree(world, |mux, world| (prompt(mux, pane), world.sent()));
        assert_eq!(outcome, InputOutcome::Declined(Retry::OnEvent), "{pane}");
        assert!(sent.is_empty(), "{pane}");
    }
}

#[test]
fn lines_and_keys_reach_their_pane() {
    let sent = agree(world, |mux, world| {
        let pane = PaneId::new("pane-shell");
        let line = mux.send(&pane, InputKind::Line("ls"));
        let keys = mux.send(
            &pane,
            InputKind::Keys {
                recipient: Recipient::Agent,
                keys: &[Key::CtrlU, Key::CtrlU],
            },
        );
        let missing = mux.send(&PaneId::new("pane-gone"), InputKind::Line("ls"));
        (line, keys, missing, world.sent())
    });
    assert_eq!(
        (sent.0, sent.1, sent.2),
        (
            InputOutcome::Delivered,
            InputOutcome::Delivered,
            InputOutcome::Declined(Retry::OnEvent)
        )
    );
    assert_eq!(sent.3.len(), 2);
}

#[test]
fn effects_change_what_later_reads_report() {
    let texts = StateTexts {
        ready: "@lead ready".into(),
        working: "@lead working".into(),
        attention: "@lead needs attention".into(),
        unknown: "@lead status unknown".into(),
    };
    let after = agree(world, |mux, _| {
        let applied = [
            mux.apply(&Effect::Identify {
                pane: PaneId::new("pane-reviewer"),
                alias: "lead".into(),
                texts: texts.clone(),
            }),
            mux.apply(&Effect::RenameTab {
                tab: TabId::new("workspace-1:tab-2"),
                label: "@lead".into(),
            }),
            mux.apply(&Effect::Badges {
                pane: PaneId::new("pane-gone"),
                badges: vec![Badge::Awaiting(1)],
            }),
            mux.apply(&Effect::ReloadKeybindings),
        ];
        let reviewer = mux
            .survey(Scope::Space(&SpaceId::new("workspace-1")), Depth::Panes)
            .map(|survey| survey.spaces[0].panes[1].clone());
        (
            applied,
            reviewer.map(|pane| (pane.title, pane.idle_label)),
            mux.tab_label(&TabId::new("workspace-1:tab-2")),
        )
    });
    assert_eq!(after.0[2], Err(MuxError::Gone(Gone::Pane)));
    assert_eq!(
        after.1,
        Ok((Some("@lead".into()), Some("@lead ready".into())))
    );
    assert_eq!(after.2, Ok(Some("@lead".into())));
}
