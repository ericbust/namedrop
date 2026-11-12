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

fn surveyed_agent(mux: &FakeMux, space: &str, pane: &str) -> Option<AgentIdentity> {
    mux.survey(Scope::Space(&SpaceId::new(space)), Depth::Panes)
        .unwrap()
        .spaces[0]
        .panes
        .iter()
        .find(|facts| facts.id == PaneId::new(pane))
        .expect("the survey lists the pane")
        .agent
        .clone()
}

#[test]
fn a_new_session_keeps_the_agent_and_makes_a_free_pane_an_agent_like_the_builder() {
    let mux = author_and_reviewer();
    mux.set_session("pane-author", "session-replacement");
    mux.set_session("pane-shell", "session-ghost");
    assert_eq!(
        surveyed_agent(&mux, "workspace-1", "pane-author"),
        Some(AgentIdentity {
            kind: AgentKind::Known(AgentRuntime::ClaudeCode),
            label: Some("claude".into()),
            session: SessionClaim::Reported(AgentSessionId::new("session-replacement")),
        })
    );
    let built = FakeMux::world()
        .space("workspace-2")
        .pane("pane-shell")
        .session("session-ghost")
        .build();
    assert_eq!(
        surveyed_agent(&mux, "workspace-2", "pane-shell"),
        surveyed_agent(&built, "workspace-2", "pane-shell")
    );
    assert_eq!(
        surveyed_agent(&mux, "workspace-2", "pane-shell"),
        Some(AgentIdentity {
            kind: AgentKind::Unreported,
            label: None,
            session: SessionClaim::Reported(AgentSessionId::new("session-ghost")),
        })
    );
}

#[test]
#[should_panic(expected = "the world has no pane pane-9")]
fn a_new_session_for_a_missing_pane_is_a_test_bug() {
    author_and_reviewer().set_session("pane-9", "session-9");
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
fn the_native_gate_declines_a_blocked_or_absent_agent_before_typing() {
    let mux = author_and_reviewer();
    let reviewer = identity(&mux, "pane-reviewer");
    mux.set_state("pane-author", AgentState::Blocked);
    for (pane, reason) in [
        ("pane-author", "pane pane-author cannot take a prompt now"),
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
fn the_native_gate_types_to_a_working_or_unknown_agent_whatever_the_admission_like_herdr() {
    let mux = author_and_reviewer();
    let reviewer = identity(&mux, "pane-reviewer");
    for admit in [Admit::Settled, Admit::BetweenTurns] {
        assert_eq!(
            prompt(&mux, "pane-reviewer", &reviewer, admit),
            InputOutcome::Delivered,
            "{admit:?}"
        );
    }
    mux.set_state("pane-reviewer", AgentState::Unknown("thinking".into()));
    assert_eq!(
        prompt(&mux, "pane-reviewer", &reviewer, Admit::Settled),
        InputOutcome::Delivered
    );
    assert_eq!(prompts(&mux, "pane-reviewer"), ["hello", "hello", "hello"]);
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
fn a_scripted_outcome_replaces_one_delivery_to_its_pane_and_records_what_may_have_landed() {
    let mux = author_and_reviewer();
    let author = identity(&mux, "pane-author");
    mux.script("pane-author", InputOutcome::Refused("invalid_input".into()));
    mux.script("pane-author", InputOutcome::Ambiguous("timed out".into()));
    mux.script("pane-shell", InputOutcome::Refused("not this pane".into()));
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
    assert_eq!(
        prompt(&mux, "pane-author", &author, Admit::Settled),
        InputOutcome::Delivered,
        "the script for another pane is not used"
    );
}

#[test]
fn an_answered_pane_gives_every_input_the_same_outcome_after_its_script() {
    let mux = author_and_reviewer();
    let author = identity(&mux, "pane-author");
    mux.answer("pane-author", InputOutcome::Refused("invalid_input".into()));
    mux.script("pane-author", InputOutcome::Delivered);
    let outcomes: Vec<_> = (0..3)
        .map(|_| prompt(&mux, "pane-author", &author, Admit::Settled))
        .collect();
    assert_eq!(
        outcomes,
        [
            InputOutcome::Delivered,
            InputOutcome::Refused("invalid_input".into()),
            InputOutcome::Refused("invalid_input".into()),
        ]
    );
    assert_eq!(prompts(&mux, "pane-author"), ["hello"]);
}

#[test]
fn every_attempt_is_recorded_with_its_outcome_even_when_nothing_was_typed() {
    let mux = author_and_reviewer();
    let reviewer = identity(&mux, "pane-reviewer");
    let keys = [Key::CtrlU; 3];
    let ghost = mux.send(
        &PaneId::new("pane-ghost"),
        InputKind::Keys {
            recipient: Recipient::Agent,
            keys: &keys,
        },
    );
    mux.set_state("pane-reviewer", AgentState::Blocked);
    let blocked = prompt(&mux, "pane-reviewer", &reviewer, Admit::Settled);
    assert!(mux.sent().is_empty());
    assert_eq!(
        mux.attempts(),
        [
            Attempt {
                pane: PaneId::new("pane-ghost"),
                input: SentInput::Keys {
                    recipient: Recipient::Agent,
                    keys: keys.to_vec()
                },
                outcome: ghost,
            },
            Attempt {
                pane: PaneId::new("pane-reviewer"),
                input: SentInput::Prompt("hello".into()),
                outcome: blocked,
            },
        ]
    );
    assert!(
        mux.attempts()
            .iter()
            .all(|attempt| matches!(attempt.outcome, InputOutcome::Declined(..)))
    );
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
fn an_effect_an_injection_fails_is_still_an_attempted_effect() {
    let mux = author_and_reviewer();
    mux.fail(Op::Apply, MuxError::Timeout("timed out".into()));
    assert_eq!(
        mux.apply(&Effect::ReloadKeybindings),
        Err(MuxError::Timeout("timed out".into()))
    );
    assert!(mux.effects().is_empty());
    assert_eq!(mux.attempted_effects(), [Effect::ReloadKeybindings]);
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
    assert_eq!(mux.attempted_effects().len(), 1, "the attempt is logged");
}

#[test]
fn a_strict_world_answers_only_the_surveys_it_names() {
    let mux = author_and_reviewer().strict_surveys(&[(ScopeKind::All, Depth::Spaces)]);
    assert_eq!(
        mux.survey(Scope::All, Depth::Spaces).unwrap().spaces.len(),
        2
    );
    assert_eq!(
        mux.survey(Scope::All, Depth::Panes),
        Err(MuxError::Unavailable(
            "the test answers no All survey at Panes depth".into()
        ))
    );
    let workspace = SpaceId::new("workspace-1");
    assert!(matches!(
        mux.survey(Scope::PanesOf(&workspace), Depth::Panes),
        Err(MuxError::Unavailable(_))
    ));
}

#[test]
#[should_panic(expected = "the world has no pane pane-9")]
fn setting_the_state_of_an_unknown_pane_panics() {
    author_and_reviewer().set_state("pane-9", AgentState::Idle);
}

#[test]
#[should_panic(expected = "the world has no pane pane-9")]
fn setting_the_screen_of_an_unknown_pane_panics() {
    author_and_reviewer().set_screen("pane-9", Some("text"));
}

#[test]
#[should_panic(expected = "the world has no pane pane-9")]
fn scripting_an_unknown_pane_panics() {
    author_and_reviewer().script("pane-9", InputOutcome::Delivered);
}

#[test]
#[should_panic(expected = "the world has no pane pane-9")]
fn answering_for_an_unknown_pane_panics() {
    author_and_reviewer().answer("pane-9", InputOutcome::Delivered);
}

#[test]
#[should_panic(expected = "the world has no pane pane-9")]
fn closing_an_unknown_pane_panics() {
    author_and_reviewer().close_pane("pane-9");
}

#[test]
#[should_panic(expected = "the world already answers only the surveys")]
fn a_clone_cannot_widen_the_surveys_a_world_answers() {
    let mux = author_and_reviewer().strict_surveys(&[(ScopeKind::All, Depth::Spaces)]);
    let _ = mux
        .clone()
        .strict_surveys(&[(ScopeKind::All, Depth::Panes)]);
}

const AS_DRAWN_ANSI: ScreenShape = ScreenShape {
    wrap: Wrap::AsDrawn,
    style: Style::Ansi,
    lines: None,
};

fn shaped(strict: bool) -> FakeMux {
    let mux = FakeMux::world()
        .space("workspace-1")
        .pane("pane-drawn")
        .screen("drawn")
        .screen_shape(AS_DRAWN_ANSI)
        .pane("pane-forty")
        .screen("joined")
        .screen_shape(ScreenShape {
            wrap: Wrap::Joined,
            style: Style::Plain,
            lines: Some(40),
        })
        .pane("pane-any")
        .screen("any")
        .build();
    if strict { mux.strict_reads() } else { mux }
}

fn read(
    mux: &FakeMux,
    pane: &str,
    lines: usize,
    wrap: Wrap,
    style: Style,
) -> Result<String, MuxError> {
    mux.screen(&PaneId::new(pane), ScreenRequest { lines, wrap, style })
        .map(|screen| screen.text)
}

#[test]
fn a_shaped_screen_answers_only_its_shape_and_strict_reads_need_one() {
    let strict = shaped(true);
    assert_eq!(
        read(&strict, "pane-drawn", 7, Wrap::AsDrawn, Style::Ansi),
        Ok("drawn".into())
    );
    assert_eq!(
        read(&strict, "pane-forty", 40, Wrap::Joined, Style::Plain),
        Ok("joined".into())
    );
    for (pane, lines, wrap, style) in [
        ("pane-drawn", 7, Wrap::Joined, Style::Ansi),
        ("pane-drawn", 7, Wrap::AsDrawn, Style::Plain),
        ("pane-forty", 39, Wrap::Joined, Style::Plain),
        ("pane-forty", 40, Wrap::AsDrawn, Style::Plain),
        ("pane-any", 40, Wrap::Joined, Style::Plain),
    ] {
        assert_eq!(
            read(&strict, pane, lines, wrap, style),
            Err(MuxError::Unavailable(format!(
                "the test gave no screen for {pane} read as {wrap:?} {style:?} with {lines} lines"
            ))),
            "a strict read of {pane} as {wrap:?} {style:?} with {lines} lines"
        );
    }
    let lenient = shaped(false);
    assert_eq!(
        read(&lenient, "pane-any", 40, Wrap::Joined, Style::Plain),
        Ok("any".into())
    );
    assert_eq!(
        read(&lenient, "pane-drawn", 7, Wrap::AsDrawn, Style::Ansi),
        Ok("drawn".into())
    );
    assert_eq!(
        read(&lenient, "pane-drawn", 7, Wrap::Joined, Style::Ansi),
        Ok(String::new())
    );
}

#[test]
fn a_screen_can_be_taken_away_and_given_back_in_the_same_shape() {
    let strict = shaped(true);
    strict.set_screen("pane-drawn", None);
    assert!(matches!(
        read(&strict, "pane-drawn", 7, Wrap::AsDrawn, Style::Ansi),
        Err(MuxError::Unavailable(_))
    ));
    strict.set_screen("pane-drawn", Some("back"));
    assert_eq!(
        read(&strict, "pane-drawn", 7, Wrap::AsDrawn, Style::Ansi),
        Ok("back".into())
    );
    assert!(read(&strict, "pane-drawn", 7, Wrap::Joined, Style::Plain).is_err());
}

#[test]
fn every_read_is_logged_with_its_shape_even_when_it_fails() {
    let mux = author_and_reviewer().strict_surveys(&[(ScopeKind::All, Depth::Spaces)]);
    let workspace = SpaceId::new("workspace-1");
    mux.fail(Op::Screen, MuxError::Timeout("timed out".into()));
    let request = ScreenRequest {
        lines: 3,
        wrap: Wrap::AsDrawn,
        style: Style::Ansi,
    };
    assert!(mux.survey(Scope::All, Depth::Spaces).is_ok());
    assert!(
        mux.survey(Scope::PanesOf(&workspace), Depth::Panes)
            .is_err()
    );
    assert!(mux.screen(&PaneId::new("pane-shell"), request).is_err());
    assert!(mux.screen(&PaneId::new("pane-shell"), request).is_ok());
    assert!(mux.process(&PaneId::new("pane-9")).is_err());
    let _ = mux.tab_label(&TabId::new("tab-1"));
    let _ = mux.integration_root();
    let shell = Read::Screen {
        pane: PaneId::new("pane-shell"),
        request,
    };
    assert_eq!(
        mux.reads(),
        [
            Read::Survey {
                scope: Surveyed::All,
                depth: Depth::Spaces
            },
            Read::Survey {
                scope: Surveyed::PanesOf(workspace),
                depth: Depth::Panes
            },
            shell.clone(),
            shell,
            Read::Process(PaneId::new("pane-9")),
            Read::TabLabel(TabId::new("tab-1")),
            Read::IntegrationRoot,
        ]
    );
    assert_eq!(mux.screen_reads("pane-shell"), 2);
    assert_eq!(mux.screen_reads("pane-author"), 0);
    assert_eq!(mux.surveys_made(ScopeKind::All, Depth::Spaces), 1);
    assert_eq!(mux.surveys_made(ScopeKind::PanesOf, Depth::Panes), 1);
    assert_eq!(mux.surveys_made(ScopeKind::PanesOf, Depth::Spaces), 0);
}

#[test]
fn a_read_hook_can_fail_every_listing_of_one_space_until_the_test_heals_it() {
    let mux = author_and_reviewer();
    let unreadable = Arc::new(Mutex::new(vec![SpaceId::new("workspace-2")]));
    let failing = Arc::clone(&unreadable);
    mux.on_read(move |_, read| match read {
        Read::Survey {
            scope: Surveyed::PanesOf(space),
            ..
        } if failing.lock().unwrap().contains(space) => Err(MuxError::Unavailable(format!(
            "{} is unavailable",
            space.as_str()
        ))),
        _ => Ok(()),
    });
    let (first, second) = (SpaceId::new("workspace-1"), SpaceId::new("workspace-2"));
    for _ in 0..2 {
        for depth in [Depth::Spaces, Depth::Panes] {
            assert_eq!(
                mux.survey(Scope::PanesOf(&second), depth),
                Err(MuxError::Unavailable("workspace-2 is unavailable".into()))
            );
            assert!(mux.survey(Scope::PanesOf(&first), depth).is_ok());
        }
    }
    unreadable.lock().unwrap().clear();
    assert!(mux.survey(Scope::PanesOf(&second), Depth::Panes).is_ok());
}

#[test]
fn a_read_hook_changes_the_world_before_the_read_answers() {
    let mux = author_and_reviewer();
    let lists = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&lists);
    mux.on_read(move |mux, read| {
        if matches!(
            read,
            Read::Survey {
                depth: Depth::Panes,
                ..
            }
        ) && counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 1
        {
            mux.set_state("pane-author", AgentState::Working);
        }
        Ok(())
    });
    let workspace = SpaceId::new("workspace-1");
    let author_state = |mux: &FakeMux| {
        mux.survey(Scope::PanesOf(&workspace), Depth::Panes)
            .unwrap()
            .spaces[0]
            .panes[0]
            .state
            .clone()
    };
    assert_eq!(author_state(&mux), Some(AgentState::Idle));
    assert_eq!(
        author_state(&mux),
        Some(AgentState::Working),
        "the second listing"
    );
    assert_eq!(Arc::strong_count(&mux.shared), 1, "no hook holds the world");
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
fn strict_reads_fail_what_the_test_did_not_give_and_answer_what_it_did() {
    let request = ScreenRequest {
        lines: 2,
        wrap: Wrap::Joined,
        style: Style::Plain,
    };
    let lenient = author_and_reviewer();
    assert_eq!(
        lenient.screen(&PaneId::new("pane-author"), request),
        Ok(Screen {
            text: String::new()
        })
    );
    assert_eq!(lenient.integration_root(), Ok(None));

    let strict = FakeMux::world()
        .space("workspace-1")
        .pane("pane-given")
        .screen("one\ntwo\nthree")
        .screen_shape(ScreenShape {
            wrap: Wrap::Joined,
            style: Style::Plain,
            lines: None,
        })
        .process(PaneProcess {
            shell_pid: Some(7),
            ..PaneProcess::default()
        })
        .tab("tab-given")
        .tab_label("tab-given", "given")
        .pane("pane-bare")
        .build()
        .strict_reads();
    fn unavailable<T>(result: Result<T, MuxError>) -> bool {
        matches!(result, Err(MuxError::Unavailable(_)))
    }
    assert!(unavailable(
        strict.screen(&PaneId::new("pane-bare"), request)
    ));
    assert!(unavailable(strict.process(&PaneId::new("pane-bare"))));
    assert!(unavailable(
        strict.tab_label(&TabId::new("workspace-1:tab-1"))
    ));
    assert!(unavailable(strict.integration_root()));
    assert_eq!(
        strict.screen(&PaneId::new("pane-given"), request),
        Ok(Screen {
            text: "two\nthree".into()
        })
    );
    assert_eq!(
        strict
            .process(&PaneId::new("pane-given"))
            .unwrap()
            .shell_pid,
        Some(7)
    );
    assert_eq!(
        strict.tab_label(&TabId::new("tab-given")),
        Ok(Some("given".into()))
    );
    assert_eq!(
        strict.screen(&PaneId::new("pane-9"), request),
        Err(MuxError::Gone(Gone::Pane, "pane-9 is gone".into()))
    );
}

#[test]
fn input_observers_see_every_attempt_with_its_outcome_before_it_returns() {
    let mux = author_and_reviewer();
    let seen = Arc::new(Mutex::new(Vec::new()));
    for observer in ["first", "second"] {
        let seen = Arc::clone(&seen);
        mux.on_input(move |mux, attempt| {
            seen.lock()
                .unwrap()
                .push((observer, attempt.clone(), mux.attempts().len()));
        });
    }
    let author = identity(&mux, "pane-author");
    let reviewer = identity(&mux, "pane-reviewer");
    mux.script("pane-shell", InputOutcome::Refused("invalid_input".into()));
    let delivered = prompt(&mux, "pane-author", &author, Admit::Settled);
    mux.set_state("pane-reviewer", AgentState::Blocked);
    let blocked = prompt(&mux, "pane-reviewer", &reviewer, Admit::Settled);
    let refused = mux.send(&PaneId::new("pane-shell"), InputKind::Line("ls"));
    let ghost = mux.send(&PaneId::new("pane-ghost"), InputKind::Line("ls"));
    let attempt = |pane: &str, input, outcome| Attempt {
        pane: PaneId::new(pane),
        input,
        outcome,
    };
    let attempts = [
        attempt("pane-author", SentInput::Prompt("hello".into()), delivered),
        attempt("pane-reviewer", SentInput::Prompt("hello".into()), blocked),
        attempt("pane-shell", SentInput::Line("ls".into()), refused),
        attempt("pane-ghost", SentInput::Line("ls".into()), ghost),
    ];
    assert_eq!(attempts[0].outcome, InputOutcome::Delivered);
    assert!(matches!(attempts[1].outcome, InputOutcome::Declined(..)));
    assert!(matches!(attempts[3].outcome, InputOutcome::Declined(..)));
    let expected: Vec<_> = attempts
        .iter()
        .enumerate()
        .flat_map(|(index, attempt)| {
            ["first", "second"].map(|observer| (observer, attempt.clone(), index + 1))
        })
        .collect();
    assert_eq!(
        *seen.lock().unwrap(),
        expected,
        "each observer sees every attempt once, in order, after it is recorded"
    );
    assert_eq!(mux.attempts(), attempts);
    assert_eq!(mux.sent().len(), 1, "only the delivered prompt was typed");
    assert_eq!(
        Arc::strong_count(&mux.shared),
        1,
        "no observer holds the world"
    );
}

#[test]
fn an_observer_can_change_what_the_pane_shows_as_keys_arrive() {
    let mux = author_and_reviewer();
    mux.set_screen("pane-author", Some("> draft"));
    mux.on_input(|mux, attempt| {
        if let SentInput::Keys { keys, .. } = &attempt.input
            && keys.iter().all(|key| *key == Key::CtrlU)
        {
            mux.set_screen(attempt.pane.as_str(), Some("> "));
        }
    });
    let request = ScreenRequest {
        lines: 1,
        wrap: Wrap::Joined,
        style: Style::Plain,
    };
    mux.send(
        &PaneId::new("pane-author"),
        InputKind::Keys {
            recipient: Recipient::Agent,
            keys: &[Key::CtrlU],
        },
    );
    assert_eq!(
        mux.screen(&PaneId::new("pane-author"), request)
            .unwrap()
            .text,
        "> "
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

fn listed_panes(mux: &FakeMux, space: &str) -> Vec<String> {
    mux.survey(Scope::PanesOf(&SpaceId::new(space)), Depth::Panes)
        .unwrap()
        .spaces[0]
        .panes
        .iter()
        .map(|pane| pane.id.as_str().to_owned())
        .collect()
}

#[test]
fn a_hidden_pane_leaves_every_listing_keeps_its_place_and_still_takes_reads_and_input() {
    let mux = author_and_reviewer();
    mux.hide_pane("pane-author");
    assert_eq!(listed_panes(&mux, "workspace-1"), ["pane-reviewer"]);
    let one = SpaceId::new("workspace-1");
    for (scope, depth) in [
        (Scope::Space(&one), Depth::Panes),
        (Scope::CheckoutOf(Path::new("/repo")), Depth::Panes),
        (Scope::All, Depth::Panes),
    ] {
        let survey = mux.survey(scope, depth).unwrap();
        assert!(
            survey
                .spaces
                .iter()
                .flat_map(|space| &space.panes)
                .all(|pane| pane.id.as_str() != "pane-author"),
            "{scope:?} {depth:?}"
        );
    }
    let author = identity(&mux, "pane-author");
    assert_eq!(
        prompt(&mux, "pane-author", &author, Admit::Settled),
        InputOutcome::Delivered
    );
    mux.show_pane("pane-author");
    assert_eq!(
        listed_panes(&mux, "workspace-1"),
        ["pane-author", "pane-reviewer"],
        "back in its old place"
    );
}

#[test]
fn a_space_whose_only_pane_is_hidden_leaves_the_all_panes_listing() {
    let mux = author_and_reviewer();
    mux.hide_pane("pane-shell");
    let survey = mux.survey(Scope::All, Depth::Panes).unwrap();
    assert!(
        survey
            .spaces
            .iter()
            .all(|space| space.id.as_str() != "workspace-2")
    );
}

#[test]
fn a_pane_hidden_by_the_builder_appears_when_shown() {
    let mux = FakeMux::world()
        .space("workspace-1")
        .pane("pane-a")
        .pane("pane-b")
        .hidden()
        .build();
    assert_eq!(listed_panes(&mux, "workspace-1"), ["pane-a"]);
    mux.show_pane("pane-b");
    assert_eq!(listed_panes(&mux, "workspace-1"), ["pane-a", "pane-b"]);
}

#[test]
fn an_opened_pane_joins_the_end_of_its_space_as_described() {
    let mux = author_and_reviewer();
    mux.open_pane("workspace-2", |space| {
        space
            .pane("pane-elsewhere")
            .terminal("t2")
            .tab("tab-9")
            .claude()
            .session("session-reviewer")
            .working()
    });
    assert_eq!(
        listed_panes(&mux, "workspace-2"),
        ["pane-shell", "pane-elsewhere"]
    );
    let opened = mux.facts("pane-elsewhere");
    assert_eq!(opened.space, SpaceId::new("workspace-2"));
    assert_eq!(opened.terminal, TerminalId::new("t2"));
    assert_eq!(opened.tab, TabId::new("tab-9"));
    assert_eq!(opened.state, Some(AgentState::Working));
    assert_eq!(
        opened.agent,
        Some(AgentIdentity {
            kind: AgentKind::Known(AgentRuntime::ClaudeCode),
            label: Some("claude".into()),
            session: SessionClaim::Reported(AgentSessionId::new("session-reviewer")),
        })
    );
}

#[test]
#[should_panic(expected = "the world already has a pane pane-author")]
fn opening_a_pane_whose_id_is_taken_panics() {
    author_and_reviewer().open_pane("workspace-2", |space| space.pane("pane-author"));
}

#[test]
#[should_panic(expected = "open_pane describes panes only")]
fn opening_a_space_through_open_pane_panics() {
    author_and_reviewer().open_pane("workspace-2", |space| space.space("workspace-9"));
}

fn checkout_path(mux: &FakeMux, scope: Scope<'_>, depth: Depth) -> Option<PathBuf> {
    mux.survey(scope, depth).unwrap().spaces[0]
        .checkout
        .as_ref()
        .map(|checkout| checkout.path.clone())
}

#[test]
fn a_checkout_can_be_removed_restored_and_moved_and_the_listing_follows_it() {
    let mux = author_and_reviewer();
    let one = SpaceId::new("workspace-1");
    mux.set_checkout("workspace-1", None);
    assert_eq!(checkout_path(&mux, Scope::Space(&one), Depth::Panes), None);
    assert_eq!(
        mux.survey(Scope::Space(&one), Depth::Panes).unwrap().spaces[0].worktree,
        Some(WorktreeId::new("herdr-space:workspace-1"))
    );
    assert!(
        mux.survey(Scope::CheckoutOf(Path::new("/repo")), Depth::Panes)
            .is_err()
    );
    mux.set_checkout("workspace-1", Some(Path::new("/repo")));
    assert_eq!(
        checkout_path(&mux, Scope::CheckoutOf(Path::new("/repo")), Depth::Spaces),
        Some("/repo".into())
    );
    mux.set_checkout("workspace-1", Some(Path::new("/moved")));
    assert_eq!(
        checkout_path(&mux, Scope::Space(&one), Depth::Spaces),
        Some("/moved".into()),
        "the listing names the moved checkout"
    );
    assert_eq!(
        checkout_path(&mux, Scope::Space(&one), Depth::Panes),
        Some("/moved".into())
    );
}

#[test]
fn a_stamp_and_an_unlisted_space_change_only_the_space_listings() {
    let mux = FakeMux::world()
        .space("w1")
        .label("one")
        .checkout("/repo")
        .stamped("/worktrees/repo/one", Some("repo"))
        .space("w2")
        .checkout("/two")
        .unstamped()
        .space("w3")
        .checkout("/three")
        .unlisted()
        .build();
    let listed = mux.survey(Scope::All, Depth::Spaces).unwrap();
    let stamps: Vec<_> = listed
        .spaces
        .iter()
        .map(|space| {
            (
                space.id.as_str().to_owned(),
                space
                    .checkout
                    .as_ref()
                    .map(|checkout| checkout.path.clone()),
                space
                    .checkout
                    .as_ref()
                    .and_then(|checkout| checkout.repo_name.clone()),
            )
        })
        .collect();
    assert_eq!(
        stamps,
        [
            (
                "w1".to_owned(),
                Some(PathBuf::from("/worktrees/repo/one")),
                Some("repo".to_owned())
            ),
            ("w2".to_owned(), None, None),
        ],
        "w3 is unlisted"
    );
    let w1 = SpaceId::new("w1");
    assert_eq!(
        checkout_path(&mux, Scope::Space(&w1), Depth::Spaces),
        Some("/worktrees/repo/one".into()),
        "workspace get names the stamp"
    );
    assert_eq!(
        checkout_path(&mux, Scope::Space(&w1), Depth::Panes),
        Some("/repo".into()),
        "worktree list --workspace names the checkout"
    );
    let w3 = SpaceId::new("w3");
    assert_eq!(
        checkout_path(&mux, Scope::Space(&w3), Depth::Panes),
        Some("/three".into()),
        "an unlisted space still answers by id"
    );
    assert_eq!(
        checkout_path(&mux, Scope::CheckoutOf(Path::new("/two")), Depth::Spaces),
        Some("/two".into())
    );
}

#[test]
fn an_agent_kind_and_a_session_come_and_go_as_herdr_reports_them() {
    let mux = author_and_reviewer();
    mux.set_agent("pane-shell", Some(AgentKind::Known(AgentRuntime::Codex)));
    assert_eq!(
        mux.facts("pane-shell").agent,
        Some(AgentIdentity {
            kind: AgentKind::Known(AgentRuntime::Codex),
            label: Some("codex".into()),
            session: SessionClaim::None,
        })
    );
    mux.set_agent("pane-author", None);
    assert_eq!(
        mux.facts("pane-author").agent,
        Some(AgentIdentity {
            kind: AgentKind::Unreported,
            label: None,
            session: SessionClaim::Reported(AgentSessionId::new("session-author")),
        }),
        "a pane that still reports a session is an agent"
    );
    mux.clear_session("pane-author");
    assert_eq!(mux.facts("pane-author").agent, None, "a free terminal pane");
    mux.clear_session("pane-reviewer");
    assert_eq!(
        mux.facts("pane-reviewer").agent,
        Some(AgentIdentity {
            kind: AgentKind::Known(AgentRuntime::Codex),
            label: Some("codex".into()),
            session: SessionClaim::None,
        }),
        "a pane that reports a kind is an agent"
    );
    mux.set_agent("pane-reviewer", None);
    assert_eq!(mux.facts("pane-reviewer").agent, None);
    mux.set_agent("pane-reviewer", Some(AgentKind::Other("aider".into())));
    assert_eq!(
        mux.facts("pane-reviewer")
            .agent
            .and_then(|agent| agent.label),
        Some("aider".into())
    );
}

#[test]
fn a_process_a_space_label_and_the_integration_root_answer_later_reads() {
    let mux = author_and_reviewer().strict_reads();
    let author = PaneId::new("pane-author");
    assert!(matches!(
        mux.process(&author),
        Err(MuxError::Unavailable(_))
    ));
    let process = PaneProcess {
        shell_pid: Some(41),
        foreground_group: Some(42),
        foreground_pids: vec![42],
        tty: Some("/dev/ttys004".into()),
    };
    mux.set_process("pane-author", Some(process.clone()));
    assert_eq!(mux.process(&author), Ok(process));
    mux.set_process("pane-author", None);
    assert!(matches!(
        mux.process(&author),
        Err(MuxError::Unavailable(_))
    ));
    assert!(matches!(
        mux.integration_root(),
        Err(MuxError::Unavailable(_))
    ));
    mux.set_integration_root(Some(Path::new("/plugins/namedrop")));
    assert_eq!(mux.integration_root(), Ok(Some("/plugins/namedrop".into())));
    mux.set_integration_root(None);
    assert!(mux.integration_root().is_err());
    let one = SpaceId::new("workspace-1");
    let label = |mux: &FakeMux| {
        mux.survey(Scope::Space(&one), Depth::Spaces)
            .unwrap()
            .spaces[0]
            .label
            .clone()
    };
    mux.set_label("workspace-1", Some("renamed"));
    assert_eq!(label(&mux), Some("renamed".into()));
    mux.set_label("workspace-1", None);
    assert_eq!(label(&mux), None);
}

#[test]
fn a_pane_can_report_no_session_field_at_all() {
    let mux = FakeMux::world()
        .space("workspace-1")
        .pane("pane-quiet")
        .claude()
        .idle()
        .reports_session(false)
        .pane("pane-loud")
        .claude()
        .idle()
        .build();
    let survey = mux
        .survey(Scope::PanesOf(&SpaceId::new("workspace-1")), Depth::Panes)
        .unwrap();
    let reports: Vec<_> = survey.spaces[0]
        .panes
        .iter()
        .map(|pane| pane.reports_session)
        .collect();
    assert_eq!(reports, [false, true]);
}

#[test]
fn every_new_setter_panics_on_an_unknown_id() {
    type Call = Box<dyn Fn(&FakeMux)>;
    let calls: [(&str, Call); 9] = [
        ("pane", Box::new(|mux| mux.hide_pane("pane-9"))),
        ("pane", Box::new(|mux| mux.show_pane("pane-9"))),
        ("pane", Box::new(|mux| mux.set_agent("pane-9", None))),
        ("pane", Box::new(|mux| mux.clear_session("pane-9"))),
        ("pane", Box::new(|mux| mux.set_process("pane-9", None))),
        (
            "pane",
            Box::new(|mux| {
                mux.facts("pane-9");
            }),
        ),
        (
            "space",
            Box::new(|mux| mux.open_pane("workspace-9", |space| space.pane("pane-new"))),
        ),
        (
            "space",
            Box::new(|mux| mux.set_checkout("workspace-9", Some(Path::new("/x")))),
        ),
        ("space", Box::new(|mux| mux.set_label("workspace-9", None))),
    ];
    for (what, call) in calls {
        let mux = author_and_reviewer();
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| call(&mux)))
            .expect_err("an unknown id panics");
        let message = panicked
            .downcast_ref::<String>()
            .cloned()
            .unwrap_or_default();
        assert!(
            message.starts_with(&format!("the world has no {what} ")),
            "{message}"
        );
    }
}

#[test]
fn a_connection_keeps_its_budget_and_cancel_flag_for_hooks_and_the_read_log() {
    let mux = author_and_reviewer();
    let budget = Budget {
        timeout: Duration::from_millis(500),
        max_output_bytes: 4096,
    };
    let cancelled = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let saw = Arc::clone(&seen);
    mux.on_read(move |connection, _| {
        saw.lock()
            .unwrap()
            .push((connection.budget(), connection.cancelled()));
        if connection.cancelled() {
            return Err(MuxError::Unavailable("cancelled".into()));
        }
        Ok(())
    });
    let connected = mux
        .connect("socket-a", budget, Arc::clone(&cancelled))
        .unwrap();
    connected.integration_root().unwrap();
    cancelled.store(true, Ordering::SeqCst);
    assert!(connected.integration_root().is_err());
    mux.integration_root().unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        [(Some(budget), false), (Some(budget), true), (None, false)]
    );
    assert_eq!(
        mux.budgeted_reads(),
        [
            (Read::IntegrationRoot, Some(budget)),
            (Read::IntegrationRoot, Some(budget)),
            (Read::IntegrationRoot, None),
        ]
    );
    assert_eq!(mux.reads().len(), 3);
}

#[test]
fn facts_inspect_the_world_without_logging_a_read_or_running_a_hook() {
    let mux = author_and_reviewer();
    mux.on_read(|_, _| panic!("facts ran a read hook"));
    mux.hide_pane("pane-author");
    assert_eq!(mux.facts("pane-author").state, Some(AgentState::Idle));
    assert!(mux.reads().is_empty());
}

/// Runs `f` on a thread and fails, rather than hangs, when it does not end
/// within five seconds, as when it waits on a lock its caller holds.
fn finishes(f: impl FnOnce() + Send + 'static) {
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        f();
        let _ = done.send(());
    });
    finished
        .recv_timeout(Duration::from_secs(5))
        .expect("finished without waiting on a lock it holds");
}

#[test]
fn hooks_and_observers_run_with_the_world_and_their_own_lists_unlocked() {
    finishes(|| {
        let mux = author_and_reviewer();
        mux.on_read(|mux, read| {
            if matches!(read, Read::IntegrationRoot) {
                mux.set_state("pane-author", AgentState::Done);
                mux.on_read(|_, _| Ok(()));
                mux.on_input(|_, _| {});
            }
            Ok(())
        });
        mux.on_input(|mux, _| {
            mux.set_screen("pane-shell", Some("observed"));
            mux.on_input(|_, _| {});
            mux.on_read(|_, _| Ok(()));
        });
        mux.integration_root().unwrap();
        assert_eq!(state(&mux, "pane-author"), Some(AgentState::Done));
        mux.send(&PaneId::new("pane-shell"), InputKind::Line("ls"));
        assert_eq!(
            mux.lock()
                .pane(&PaneId::new("pane-shell"))
                .and_then(|pane| pane.screen.clone()),
            Some("observed".into())
        );
    });
}

#[test]
fn a_read_within_a_budget_logs_the_tighter_of_it_and_the_connection_s() {
    let mux = author_and_reviewer();
    let budget = |millis, bytes| Budget {
        timeout: Duration::from_millis(millis),
        max_output_bytes: bytes,
    };
    mux.within(budget(2000, 65536)).integration_root().unwrap();
    let connected = mux
        .connect("socket-a", budget(500, 1 << 20), Arc::default())
        .unwrap();
    connected
        .within(budget(2000, 65536))
        .integration_root()
        .unwrap();
    assert_eq!(
        mux.budgeted_reads(),
        [
            (Read::IntegrationRoot, Some(budget(2000, 65536))),
            (Read::IntegrationRoot, Some(budget(500, 65536))),
        ]
    );
}

#[test]
fn an_apply_hook_fails_the_effects_it_names_and_each_stays_attempted() {
    let mux = author_and_reviewer();
    mux.on_apply(|mux, effect| match effect {
        Effect::ReloadKeybindings => {
            mux.set_state("pane-author", AgentState::Done);
            Err(MuxError::Unavailable(
                "server reload-config is unexpected".into(),
            ))
        }
        _ => Ok(()),
    });
    assert_eq!(
        mux.apply(&Effect::ReloadKeybindings),
        Err(MuxError::Unavailable(
            "server reload-config is unexpected".into()
        ))
    );
    let rename = Effect::RenameTab {
        tab: TabId::new("workspace-1:tab-1"),
        label: "@author".into(),
    };
    assert_eq!(mux.apply(&rename), Ok(Applied::Done));
    assert_eq!(state(&mux, "pane-author"), Some(AgentState::Done));
    assert_eq!(mux.effects(), std::slice::from_ref(&rename));
    assert_eq!(mux.attempted_effects(), [Effect::ReloadKeybindings, rename]);
    assert_eq!(Arc::strong_count(&mux.shared), 1, "no hook holds the world");
}
