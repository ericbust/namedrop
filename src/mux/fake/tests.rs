use super::*;
use crate::mux::{
    Admit, Badge, Budget, Gone, Incarnation, PopupScreen, PopupSpec, PromptRequest, StateTexts,
    Style, Wrap,
};

fn author_and_reviewer() -> FakeMux {
    FakeMux::world()
        .endpoint("socket-a")
        .space("workspace-1")
        .label("main")
        .checkout("/repo")
        .branch("main")
        .pane("pane-author")
        .claude()
        .session("session-author")
        .idle()
        .pane("pane-reviewer")
        .codex()
        .session("session-reviewer")
        .working()
        .space("workspace-2")
        .pane("pane-shell")
        .screen("one\ntwo\nthree")
        .build()
}

fn identity(mux: &FakeMux, pane: &str) -> AgentIdentity {
    mux.lock()
        .pane(&PaneId::new(pane))
        .and_then(|pane| pane.facts.agent.clone())
        .expect("the pane runs an agent")
}

fn prompt(mux: &FakeMux, pane: &str, expect: &AgentIdentity, admit: Admit) -> InputOutcome {
    let request = PromptRequest {
        expect,
        text: "hello",
        admit,
    };
    mux.send(&PaneId::new(pane), InputKind::Prompt(request))
}

fn state(mux: &FakeMux, pane: &str) -> Option<AgentState> {
    mux.lock()
        .pane(&PaneId::new(pane))
        .and_then(|pane| pane.facts.state.clone())
}

fn prompts(mux: &FakeMux, pane: &str) -> Vec<String> {
    mux.sent_to(pane)
        .into_iter()
        .filter_map(|sent| match sent.input {
            SentInput::Prompt(text) => Some(text),
            _ => None,
        })
        .collect()
}

#[test]
fn a_space_survey_reports_its_checkout_and_each_pane_agent() {
    let mux = author_and_reviewer();
    let survey = mux
        .survey(Scope::Space(&SpaceId::new("workspace-1")), Depth::Panes)
        .unwrap();
    let [space] = survey.spaces.as_slice() else {
        panic!("one space: {survey:?}");
    };
    assert_eq!(space.label, None, "a checkout listing names no space label");
    assert_eq!(space.worktree, Some(WorktreeId::new("/repo")));
    assert_eq!(
        space.checkout.as_ref().and_then(|c| c.branch.as_deref()),
        Some("main")
    );
    let author = &space.panes[0];
    assert_eq!(author.id, PaneId::new("pane-author"));
    assert_eq!(author.state, Some(AgentState::Idle));
    assert_eq!(
        author.agent,
        Some(AgentIdentity {
            kind: AgentKind::Known(AgentRuntime::ClaudeCode),
            label: Some("claude".into()),
            session: SessionClaim::Reported(AgentSessionId::new("session-author")),
        })
    );
    assert_eq!(space.panes[1].state, Some(AgentState::Working));
}

#[test]
fn a_space_without_a_checkout_is_keyed_by_its_id_and_holds_free_panes() {
    let mux = author_and_reviewer();
    let survey = mux
        .survey(Scope::Space(&SpaceId::new("workspace-2")), Depth::Panes)
        .unwrap();
    let space = &survey.spaces[0];
    assert_eq!(space.checkout, None);
    assert_eq!(
        space.worktree,
        Some(WorktreeId::new("herdr-space:workspace-2"))
    );
    assert_eq!(space.panes[0].agent, None, "a free terminal pane");
}

#[test]
fn a_session_without_an_agent_kind_is_still_an_agent() {
    let mux = FakeMux::world()
        .space("workspace-1")
        .pane("pane-1")
        .session("session-1")
        .build();
    assert_eq!(
        identity(&mux, "pane-1"),
        AgentIdentity {
            kind: AgentKind::Unreported,
            label: None,
            session: SessionClaim::Reported(AgentSessionId::new("session-1")),
        }
    );
}

#[test]
fn a_replaced_agent_reports_its_new_session_and_a_free_pane_stays_free() {
    let mux = author_and_reviewer();
    mux.set_session("pane-author", "session-replacement");
    mux.set_session("pane-shell", "session-ghost");
    assert_eq!(
        identity(&mux, "pane-author").session,
        SessionClaim::Reported(AgentSessionId::new("session-replacement"))
    );
    let survey = mux
        .survey(Scope::Space(&SpaceId::new("workspace-2")), Depth::Panes)
        .unwrap();
    assert_eq!(survey.spaces[0].panes[0].agent, None);
}

#[test]
fn surveys_read_only_what_their_scope_and_depth_ask_for() {
    let mux = author_and_reviewer();
    let summary = mux
        .survey(Scope::Space(&SpaceId::new("workspace-1")), Depth::Spaces)
        .unwrap();
    let space = &summary.spaces[0];
    assert_eq!(
        (space.label.as_deref(), &space.worktree, space.panes.len()),
        (Some("main"), &None, 0)
    );
    let spaces = mux.survey(Scope::All, Depth::Spaces).unwrap().spaces;
    assert_eq!(
        spaces
            .iter()
            .map(|space| (space.id.as_str(), space.number, space.panes.len()))
            .collect::<Vec<_>>(),
        [("workspace-1", Some(1), 0), ("workspace-2", Some(2), 0)]
    );
    let panes = mux.survey(Scope::All, Depth::Panes).unwrap().spaces;
    assert_eq!(
        panes
            .iter()
            .map(|space| (space.id.as_str(), space.label.clone(), space.panes.len()))
            .collect::<Vec<_>>(),
        [("workspace-1", None, 2), ("workspace-2", None, 1)]
    );
    let checkout = mux
        .survey(Scope::CheckoutOf(Path::new("/repo")), Depth::Panes)
        .unwrap();
    assert_eq!(checkout.spaces[0].id, SpaceId::new("workspace-1"));
    assert_eq!(checkout.spaces[0].panes.len(), 2);
}

#[test]
fn a_checkout_held_by_two_spaces_or_none_is_an_error() {
    let mux = FakeMux::world()
        .space("workspace-1")
        .checkout("/repo")
        .space("workspace-2")
        .checkout("/repo")
        .build();
    for path in ["/repo", "/elsewhere"] {
        assert!(matches!(
            mux.survey(Scope::CheckoutOf(Path::new(path)), Depth::Panes),
            Err(MuxError::Unavailable(_))
        ));
    }
}

#[test]
fn a_missing_space_or_pane_is_gone() {
    let mux = author_and_reviewer();
    assert_eq!(
        mux.survey(Scope::Space(&SpaceId::new("workspace-9")), Depth::Panes),
        Err(MuxError::Gone(Gone::Space, "workspace-9 is gone".into()))
    );
    assert_eq!(
        mux.process(&PaneId::new("pane-9")),
        Err(MuxError::Gone(Gone::Pane, "pane-9 is gone".into()))
    );
}

#[test]
fn a_settled_agent_takes_a_prompt_and_starts_working() {
    let mux = author_and_reviewer();
    let author = identity(&mux, "pane-author");
    assert_eq!(
        prompt(&mux, "pane-author", &author, Admit::Settled),
        InputOutcome::Delivered
    );
    assert_eq!(prompts(&mux, "pane-author"), ["hello"]);
    assert_eq!(state(&mux, "pane-author"), Some(AgentState::Working));
}

#[test]
fn the_native_gate_declines_a_busy_or_absent_agent_before_typing() {
    let mux = author_and_reviewer();
    let reviewer = identity(&mux, "pane-reviewer");
    for (pane, reason) in [
        (
            "pane-reviewer",
            "pane pane-reviewer cannot take a prompt now",
        ),
        ("pane-shell", "pane pane-shell cannot take a prompt now"),
        ("pane-gone", "pane pane-gone is gone"),
    ] {
        assert_eq!(
            prompt(&mux, pane, &reviewer, Admit::Settled),
            InputOutcome::Declined(Retry::OnEvent, reason.into()),
            "{pane}"
        );
    }
    assert!(mux.sent().is_empty());
}

#[test]
fn the_native_gate_honors_a_between_turns_admission_like_herdr() {
    let mux = author_and_reviewer();
    let reviewer = identity(&mux, "pane-reviewer");
    assert_eq!(
        prompt(&mux, "pane-reviewer", &reviewer, Admit::BetweenTurns),
        InputOutcome::Delivered
    );
    assert_eq!(prompts(&mux, "pane-reviewer"), ["hello"]);
}

#[test]
fn the_emulated_gate_declines_between_turns_and_a_replaced_agent() {
    let mux = author_and_reviewer().emulated();
    let reviewer = identity(&mux, "pane-reviewer");
    assert_eq!(
        prompt(&mux, "pane-reviewer", &reviewer, Admit::BetweenTurns),
        InputOutcome::Declined(
            Retry::OnEvent,
            "pane pane-reviewer cannot take a prompt now".into()
        )
    );
    let replaced = AgentIdentity {
        session: SessionClaim::Unreported {
            incarnation: Incarnation("4242".into()),
        },
        ..identity(&mux, "pane-author")
    };
    assert_eq!(
        prompt(&mux, "pane-author", &replaced, Admit::Settled),
        InputOutcome::Declined(
            Retry::OnEvent,
            "pane pane-author cannot take a prompt now".into()
        )
    );
    assert!(mux.sent().is_empty());
}

#[test]
fn the_emulated_gate_confirms_a_prompt_by_the_change_to_working() {
    let mux = author_and_reviewer().emulated();
    let author = identity(&mux, "pane-author");
    assert_eq!(
        prompt(&mux, "pane-author", &author, Admit::Settled),
        InputOutcome::Delivered
    );
    assert_eq!(prompts(&mux, "pane-author"), ["hello"]);
}

#[test]
fn an_emulated_prompt_that_is_never_confirmed_or_fails_is_ambiguous() {
    let unconfirmed = author_and_reviewer().emulated().prompts_stay_settled();
    let failing = author_and_reviewer().emulated();
    failing.fail(Op::Deliver, MuxError::Unavailable("reset".into()));
    for mux in [unconfirmed, failing] {
        let author = identity(&mux, "pane-author");
        assert!(matches!(
            prompt(&mux, "pane-author", &author, Admit::Settled),
            InputOutcome::Ambiguous(_)
        ));
        assert_eq!(prompts(&mux, "pane-author"), ["hello"]);
    }
}

#[test]
fn lines_and_keys_are_recorded_for_their_pane() {
    let mux = author_and_reviewer();
    let pane = PaneId::new("pane-shell");
    assert_eq!(
        mux.send(&pane, InputKind::Line("ls")),
        InputOutcome::Delivered
    );
    let keys = [Key::CtrlU, Key::Enter];
    assert_eq!(
        mux.send(
            &pane,
            InputKind::Keys {
                recipient: Recipient::Pane,
                keys: &keys
            }
        ),
        InputOutcome::Delivered
    );
    assert_eq!(
        mux.sent_to("pane-shell"),
        [
            Sent {
                pane: pane.clone(),
                input: SentInput::Line("ls".into())
            },
            Sent {
                pane,
                input: SentInput::Keys {
                    recipient: Recipient::Pane,
                    keys: keys.to_vec()
                }
            },
        ]
    );
}

#[test]
fn a_scripted_outcome_replaces_one_delivery_and_records_what_may_have_landed() {
    let mux = author_and_reviewer();
    let author = identity(&mux, "pane-author");
    mux.script(InputOutcome::Refused("invalid_input".into()));
    mux.script(InputOutcome::Ambiguous("timed out".into()));
    assert_eq!(
        prompt(&mux, "pane-author", &author, Admit::Settled),
        InputOutcome::Refused("invalid_input".into())
    );
    assert!(prompts(&mux, "pane-author").is_empty());
    assert_eq!(
        prompt(&mux, "pane-author", &author, Admit::Settled),
        InputOutcome::Ambiguous("timed out".into())
    );
    assert_eq!(prompts(&mux, "pane-author"), ["hello"]);
    assert_eq!(state(&mux, "pane-author"), Some(AgentState::Idle));
}

#[test]
fn an_injected_error_fails_the_next_call_only() {
    let mux = author_and_reviewer();
    mux.fail(Op::Survey, MuxError::Timeout("timed out".into()));
    assert_eq!(
        mux.survey(Scope::All, Depth::Spaces),
        Err(MuxError::Timeout("timed out".into()))
    );
    assert!(mux.survey(Scope::All, Depth::Spaces).is_ok());
}

#[test]
fn applied_effects_are_logged_and_visible_to_later_reads() {
    let mux = author_and_reviewer();
    let tab = TabId::new("workspace-1:tab-1");
    let texts = StateTexts {
        ready: "@author ready".into(),
        working: "@author working".into(),
        attention: "@author needs attention".into(),
        unknown: "@author status unknown".into(),
    };
    let effects = [
        Effect::Identify {
            pane: PaneId::new("pane-author"),
            alias: "author".into(),
            texts,
        },
        Effect::RenameTab {
            tab: tab.clone(),
            label: "@author".into(),
        },
    ];
    for effect in &effects {
        assert_eq!(mux.apply(effect), Ok(Applied::Done));
    }
    assert_eq!(mux.effects(), effects);
    assert_eq!(mux.tab_label(&tab), Ok(Some("@author".into())));
    let author = mux
        .lock()
        .pane(&PaneId::new("pane-author"))
        .unwrap()
        .facts
        .clone();
    assert_eq!(author.title.as_deref(), Some("@author"));
    assert_eq!(author.idle_label.as_deref(), Some("@author ready"));
}

#[test]
fn an_opened_popup_names_its_target_and_is_logged() {
    let mux = author_and_reviewer();
    let target = mux
        .lock()
        .pane(&PaneId::new("pane-author"))
        .unwrap()
        .facts
        .clone();
    let popup = Effect::OpenPopup(PopupSpec {
        screen: PopupScreen::Doctor,
        target,
        state_dir: "/state".into(),
    });
    assert_eq!(
        mux.apply(&popup),
        Ok(Applied::Opened(
            serde_json::json!({"opened_for": "pane-author"})
        ))
    );
    assert_eq!(mux.effects(), [popup]);
}

#[test]
fn an_effect_on_a_closed_pane_is_gone_and_not_logged() {
    let mux = author_and_reviewer();
    mux.close_pane("pane-author");
    assert_eq!(
        mux.apply(&Effect::Badges {
            pane: PaneId::new("pane-author"),
            badges: vec![Badge::Awaiting(1)],
        }),
        Err(MuxError::Gone(Gone::Pane, "pane-author is gone".into()))
    );
    assert!(mux.effects().is_empty());
}

#[test]
fn a_screen_read_returns_the_last_lines() {
    let mux = author_and_reviewer();
    let request = ScreenRequest {
        lines: 2,
        wrap: Wrap::Joined,
        style: Style::Plain,
    };
    assert_eq!(
        mux.screen(&PaneId::new("pane-shell"), request),
        Ok(Screen {
            text: "two\nthree".into()
        })
    );
}

#[test]
fn a_connection_to_the_fake_endpoint_shares_its_world() {
    let mux = author_and_reviewer();
    let budget = Budget {
        timeout: Duration::from_secs(1),
        max_output_bytes: 1024,
    };
    assert!(matches!(
        mux.connect("socket-b", budget, Arc::default()),
        Err(MuxError::Unavailable(_))
    ));
    let connected = mux.connect("socket-a", budget, Arc::default()).unwrap();
    connected.apply(&Effect::ReloadKeybindings).unwrap();
    assert_eq!(mux.effects(), [Effect::ReloadKeybindings]);
}
