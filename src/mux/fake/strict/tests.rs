use super::*;
use crate::mux::{
    Badge, Budget, InputKind, InputOutcome, Key, MuxAct, MuxConnector, MuxError, MuxRead, PaneId,
    PromptRequest, Recipient, Scope, ScreenRequest, StateTexts, Style, TabId, Wrap,
    fake::{Op, ScreenShape},
};
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    path::Path,
    process::Command,
    sync::mpsc,
    time::Duration,
};

fn author() -> FakeMux {
    FakeMux::world()
        .endpoint("socket-a")
        .space("workspace-1")
        .checkout("/repo")
        .pane("pane-author")
        .claude()
        .session("session-author")
        .idle()
        .screen_shape(ScreenShape {
            wrap: Wrap::Joined,
            style: Style::Plain,
            lines: Some(5),
        })
        .build()
}

fn space(takes: Takes) -> StrictSpace {
    StrictSpace::new(author(), CHECKOUT_LISTINGS, takes)
}

const REQUEST: ScreenRequest = ScreenRequest {
    lines: 5,
    wrap: Wrap::Joined,
    style: Style::Plain,
};

fn prompt(space: &FakeMux, pane: &str) -> InputOutcome {
    let expect = space
        .lock()
        .pane(&PaneId::new(pane))
        .and_then(|pane| pane.facts.agent.clone())
        .expect("the pane runs an agent");
    space.send(
        &PaneId::new(pane),
        InputKind::Prompt(PromptRequest {
            expect: &expect,
            text: "hello",
        }),
    )
}

fn delete_three_lines(space: &StrictSpace, pane: &str) -> InputOutcome {
    space.send(
        &PaneId::new(pane),
        InputKind::Keys {
            recipient: Recipient::Agent,
            keys: &[Key::CtrlU; 3],
        },
    )
}

fn notify() -> Effect {
    Effect::Notify {
        title: "stray".into(),
        body: "stray".into(),
        sound: false,
    }
}

fn badges() -> Effect {
    Effect::Badges {
        pane: PaneId::new("pane-author"),
        badges: vec![Badge::Awaiting(1)],
    }
}

fn identify() -> Effect {
    Effect::Identify {
        pane: PaneId::new("pane-author"),
        alias: "author".into(),
        texts: StateTexts {
            ready: "ready".into(),
            working: "working".into(),
            attention: "attention".into(),
            unknown: "unknown".into(),
        },
    }
}

/// What the space says as it drops: `None` when it drops quietly, and its
/// panic message when it fails the test.
fn verdict(space: StrictSpace) -> Option<String> {
    catch_unwind(AssertUnwindSafe(|| drop(space)))
        .err()
        .map(|payload| {
            *payload
                .downcast::<String>()
                .expect("the space panics with its report")
        })
}

#[test]
fn a_space_that_sees_only_what_it_takes_drops_quietly() {
    let space = space(Takes::prompts_and_metadata());
    space.set_screen("pane-author", Some("ready"));
    let surveyed = space
        .survey(Scope::CheckoutOf(Path::new("/repo")), Depth::Panes)
        .unwrap();
    assert_eq!(surveyed.spaces[0].panes.len(), 1);
    assert_eq!(
        space
            .screen(&PaneId::new("pane-author"), REQUEST)
            .unwrap()
            .text,
        "ready"
    );
    assert_eq!(prompt(&space, "pane-author"), InputOutcome::Delivered);
    space.apply(&badges()).unwrap();
}

#[test]
#[should_panic(expected = "input the space does not take")]
fn input_the_space_does_not_take_fails_the_test() {
    let space = space(Takes::prompts_and_metadata());
    assert_eq!(
        space.send(&PaneId::new("pane-author"), InputKind::Line("ls")),
        InputOutcome::Delivered
    );
}

#[test]
#[should_panic(expected = "an effect the space does not take: Notify")]
fn an_effect_the_space_does_not_take_fails_the_test() {
    let space = space(Takes::prompts_and_metadata());
    space.apply(&notify()).unwrap();
}

#[test]
#[should_panic(expected = "the test answers no Space survey at Spaces depth")]
fn a_survey_the_space_does_not_answer_fails_when_it_is_made() {
    let space = space(Takes::nothing());
    let workspace = crate::SpaceId::new("workspace-1");
    space
        .survey(Scope::Space(&workspace), Depth::Spaces)
        .unwrap();
}

#[test]
#[should_panic(expected = "the test gave no screen for pane-author")]
fn a_read_the_test_did_not_give_fails_when_it_is_made() {
    let space = space(Takes::nothing());
    space.screen(&PaneId::new("pane-author"), REQUEST).unwrap();
}

#[test]
fn a_screen_answers_only_the_shape_the_old_fake_answered() {
    let space = space(Takes::nothing());
    space.set_screen("pane-author", Some("ready"));
    for request in [
        ScreenRequest {
            lines: 4,
            ..REQUEST
        },
        ScreenRequest {
            wrap: Wrap::AsDrawn,
            ..REQUEST
        },
        ScreenRequest {
            style: Style::Ansi,
            ..REQUEST
        },
    ] {
        assert!(
            matches!(
                space.screen(&PaneId::new("pane-author"), request),
                Err(MuxError::Unavailable(_))
            ),
            "{request:?}"
        );
    }
}

#[test]
fn each_listing_answers_exactly_its_surveys() {
    let workspace = crate::SpaceId::new("workspace-1");
    let scopes = [
        (ScopeKind::Space, Scope::Space(&workspace)),
        (ScopeKind::PanesOf, Scope::PanesOf(&workspace)),
        (ScopeKind::CheckoutOf, Scope::CheckoutOf(Path::new("/repo"))),
        (ScopeKind::All, Scope::All),
    ];
    let answered = |listings: &[(ScopeKind, Depth)]| {
        let space = StrictSpace::new(author(), listings, Takes::nothing());
        let mut answered = Vec::new();
        for (kind, scope) in scopes {
            for depth in [Depth::Spaces, Depth::Panes] {
                match space.survey(scope, depth) {
                    Ok(_) => answered.push((kind, depth)),
                    Err(error) => assert_eq!(
                        error,
                        MuxError::Unavailable(format!(
                            "the test answers no {kind:?} survey at {depth:?} depth"
                        ))
                    ),
                }
            }
        }
        answered
    };
    assert_eq!(
        answered(CHECKOUT_LISTINGS),
        [
            (ScopeKind::PanesOf, Depth::Spaces),
            (ScopeKind::PanesOf, Depth::Panes),
            (ScopeKind::CheckoutOf, Depth::Spaces),
            (ScopeKind::CheckoutOf, Depth::Panes),
            (ScopeKind::All, Depth::Spaces),
        ],
        "workspace list, worktree list --cwd, and pane list --workspace"
    );
    assert_eq!(
        answered(EVERY_LISTING),
        [
            (ScopeKind::Space, Depth::Panes),
            (ScopeKind::PanesOf, Depth::Spaces),
            (ScopeKind::PanesOf, Depth::Panes),
            (ScopeKind::CheckoutOf, Depth::Spaces),
            (ScopeKind::CheckoutOf, Depth::Panes),
            (ScopeKind::All, Depth::Spaces),
            (ScopeKind::All, Depth::Panes),
        ],
        "every listing, and never workspace get"
    );
}

#[test]
fn each_listing_constant_is_every_survey_whose_herdr_listings_its_fake_answered() {
    let pairs = [
        ScopeKind::Space,
        ScopeKind::PanesOf,
        ScopeKind::CheckoutOf,
        ScopeKind::All,
    ]
    .into_iter()
    .flat_map(|kind| [(kind, Depth::Spaces), (kind, Depth::Panes)]);
    let within = |answered: &[Listing]| -> Vec<(ScopeKind, Depth)> {
        pairs
            .clone()
            .filter(|(kind, depth)| {
                listings(*kind, *depth)
                    .iter()
                    .all(|listing| answered.contains(listing))
            })
            .collect()
    };
    let sorted = |pairs: &[(ScopeKind, Depth)]| {
        let mut pairs: Vec<_> = pairs.iter().map(|pair| format!("{pair:?}")).collect();
        pairs.sort();
        pairs
    };
    assert_eq!(
        sorted(&within(&[
            Listing::Spaces,
            Listing::CheckoutAt,
            Listing::PanesOf
        ])),
        sorted(CHECKOUT_LISTINGS)
    );
    assert_eq!(
        sorted(&within(&[
            Listing::Spaces,
            Listing::CheckoutAt,
            Listing::CheckoutOf,
            Listing::AllPanes,
            Listing::PanesOf
        ])),
        sorted(EVERY_LISTING)
    );
    let space = crate::SpaceId::new("workspace-1");
    let read = |scope: Scope<'_>, depth| Read::Survey {
        scope: super::super::Surveyed::of(scope),
        depth,
    };
    assert!(runs(
        &read(Scope::Space(&space), Depth::Panes),
        Listing::PanesOf
    ));
    assert!(!runs(&read(Scope::All, Depth::Panes), Listing::PanesOf));
    assert!(!runs(&Read::IntegrationRoot, Listing::PanesOf));
}

#[test]
fn prompts_and_metadata_takes_prompts_identity_and_badges_and_nothing_else() {
    let taken = space(Takes::prompts_and_metadata());
    prompt(&taken, "pane-author");
    taken.apply(&identify()).unwrap();
    taken.apply(&badges()).unwrap();
    assert_eq!(verdict(taken), None);

    let refused_input = [
        InputKind::Line("ls"),
        InputKind::Keys {
            recipient: Recipient::Agent,
            keys: &[Key::CtrlU],
        },
    ];
    for kind in refused_input {
        let space = space(Takes::prompts_and_metadata());
        space.send(&PaneId::new("pane-author"), kind);
        let attempt = space.attempts().remove(0);
        assert_eq!(
            verdict(space),
            Some(format!(
                "the strict space saw 1 violation(s):\ninput the space does not take: {attempt:?}"
            ))
        );
    }
    let refused_effects = [
        notify(),
        Effect::RenameTab {
            tab: TabId::new("tab-1"),
            label: "main".into(),
        },
        Effect::ReloadKeybindings,
    ];
    for effect in refused_effects {
        let space = space(Takes::prompts_and_metadata());
        space.apply(&effect).unwrap();
        assert_eq!(
            verdict(space),
            Some(format!(
                "the strict space saw 1 violation(s):\nan effect the space does not take: {effect:?}"
            ))
        );
    }
}

#[test]
fn every_allowance_adds_to_the_ones_before_it() {
    let takes = Takes::nothing()
        .input(|attempt| matches!(attempt.input, SentInput::Prompt(_)))
        .input(|attempt| matches!(attempt.input, SentInput::Keys { .. }))
        .effect(|effect| matches!(effect, Effect::Notify { .. }))
        .effect(|effect| matches!(effect, Effect::ReloadKeybindings));
    let space = space(takes);
    prompt(&space, "pane-author");
    delete_three_lines(&space, "pane-author");
    space.apply(&notify()).unwrap();
    space.apply(&Effect::ReloadKeybindings).unwrap();
    assert_eq!(verdict(space), None);
}

#[test]
#[should_panic(expected = "input the space does not take: Attempt { pane: PaneId(\"pane-ghost\")")]
fn input_declined_at_a_missing_pane_still_fails_a_space_that_takes_nothing() {
    let space = space(Takes::nothing());
    assert!(matches!(
        delete_three_lines(&space, "pane-ghost"),
        InputOutcome::Declined(..)
    ));
    assert!(space.sent().is_empty(), "nothing was typed");
}

#[test]
#[should_panic(expected = "outcome: Refused(\"invalid_input\")")]
fn input_refused_by_a_script_still_fails_a_space_that_takes_nothing() {
    let space = space(Takes::nothing());
    space.script("pane-author", InputOutcome::Refused("invalid_input".into()));
    assert_eq!(
        prompt(&space, "pane-author"),
        InputOutcome::Refused("invalid_input".into())
    );
}

#[test]
#[should_panic(expected = "an effect the space does not take: Badges")]
fn an_effect_that_failed_still_fails_a_space_that_takes_nothing() {
    let space = space(Takes::nothing());
    space.close_pane("pane-author");
    assert!(space.apply(&badges()).is_err());
}

#[test]
fn an_effect_an_injection_failed_still_fails_a_space_that_takes_nothing() {
    let space = space(Takes::nothing());
    space.fail(Op::Apply, MuxError::Timeout("timed out".into()));
    assert!(space.apply(&notify()).is_err());
    assert_eq!(
        verdict(space),
        Some(format!(
            "the strict space saw 1 violation(s):\nan effect the space does not take: {:?}",
            notify()
        ))
    );
}

#[test]
fn the_report_names_every_violation_in_full() {
    let space = space(Takes::nothing());
    prompt(&space, "pane-author");
    delete_three_lines(&space, "pane-author");
    space.apply(&notify()).unwrap();
    space.script("pane-author", InputOutcome::Delivered);
    space.fail(Op::Process, MuxError::Timeout("timed out".into()));
    let budget = Budget {
        timeout: Duration::from_secs(1),
        max_output_bytes: 1024,
    };
    let connection = space.connect("socket-a", budget, Arc::default()).unwrap();
    let report = verdict(space);
    drop(connection);
    assert_eq!(
        report.as_deref(),
        Some(
            "the strict space saw 6 violation(s):\n\
             input the space does not take: Attempt { pane: PaneId(\"pane-author\"), input: \
             Prompt(\"hello\"), outcome: Delivered }\n\
             input the space does not take: Attempt { pane: PaneId(\"pane-author\"), input: \
             Keys { recipient: Agent, keys: [CtrlU, CtrlU, CtrlU] }, outcome: Delivered }\n\
             an effect the space does not take: Notify { title: \"stray\", body: \"stray\", \
             sound: false }\n\
             a script for pane-author that no input used: [Delivered]\n\
             an injected Process failure that no call hit: Timeout(\"timed out\")\n\
             1 other handle(s) to the world outlived the space, so what they do next goes \
             unchecked: a connection, a clone, or a thread still holds one"
        )
    );
}

#[test]
fn used_scripts_and_hit_injections_drop_quietly() {
    let space = space(Takes::prompts_and_metadata());
    space.script("pane-author", InputOutcome::Delivered);
    space.answer("pane-author", InputOutcome::Delivered);
    space.fail(Op::Survey, MuxError::Timeout("timed out".into()));
    prompt(&space, "pane-author");
    assert!(space.survey(Scope::All, Depth::Spaces).is_err());
    assert_eq!(verdict(space), None, "an answer no input used is fine");
}

#[test]
#[should_panic(expected = "1 other handle(s) to the world outlived the space")]
fn a_connection_that_outlives_the_space_fails_the_test() {
    let space = space(Takes::nothing());
    let budget = Budget {
        timeout: Duration::from_secs(1),
        max_output_bytes: 1024,
    };
    let _connection = space.connect("socket-a", budget, Arc::default()).unwrap();
    drop(space);
}

#[test]
fn clones_that_observers_hooks_and_predicates_capture_do_not_outlive_the_space() {
    let mux = author();
    let reader = mux.clone();
    let takes = Takes::nothing()
        .input(move |attempt| {
            reader
                .sent_to(attempt.pane.as_str())
                .iter()
                .any(|sent| sent.input == attempt.input)
        })
        .effect(|effect| *effect == Effect::ReloadKeybindings);
    let space = StrictSpace::new(mux, CHECKOUT_LISTINGS, takes);
    let observed = space.clone();
    space.on_input(move |_, _| drop(observed.sent()));
    let hooked = space.clone();
    space.on_read(move |_, _| {
        drop(hooked.reads());
        Ok(())
    });
    let applying = space.clone();
    space.on_apply(move |_, _| {
        drop(applying.effects());
        Err(MuxError::Unavailable("refused".into()))
    });
    let commanded = space.clone();
    space.on_command(move |_, _| {
        drop(commanded.calls());
        Ok(())
    });
    let deciding = space.clone();
    space.answer_with(move |_, _, _| {
        drop(deciding.attempts());
        None
    });
    prompt(&space, "pane-author");
    let _ = space.process(&PaneId::new("pane-author"));
    let _ = space.apply(&Effect::ReloadKeybindings);
    let (done, verdict_of) = mpsc::channel();
    std::thread::spawn(move || done.send(verdict(space)).unwrap());
    assert_eq!(
        verdict_of
            .recv_timeout(Duration::from_secs(10))
            .expect("a predicate that reads the fake does not hang the drop"),
        None
    );
}

#[test]
#[should_panic(expected = "the world already answers only the surveys")]
fn a_clone_cannot_make_a_second_space_with_wider_surveys() {
    let space = space(Takes::nothing());
    let _wider = StrictSpace::new(space.clone(), EVERY_LISTING, Takes::nothing());
}

#[test]
#[should_panic(expected = "the test's own failure")]
fn a_failing_test_keeps_its_own_message_when_the_space_also_fails() {
    let space = space(Takes::nothing());
    space.apply(&notify()).unwrap();
    panic!("the test's own failure");
}

const CHILD: &str = "NAMEDROP_STRICT_SPACE_CHILD";

/// Runs itself in a child test process with `CHILD` set, where the space
/// drops while the test panics, so the report must reach stderr without a
/// second panic that would abort the child.
#[test]
fn a_space_that_drops_while_its_test_panics_prints_its_report() {
    if std::env::var_os(CHILD).is_some() {
        let space = space(Takes::nothing());
        space.apply(&notify()).unwrap();
        panic!("the child's own failure");
    }
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "mux::fake::strict::tests::a_space_that_drops_while_its_test_panics_prints_its_report",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(101),
        "the child failed, not aborted: {stderr}"
    );
    assert!(stderr.contains("the child's own failure"), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "the strict space saw 1 violation(s):\nan effect the space does not take: {:?}\n",
            notify()
        )),
        "{stderr}"
    );
}

#[test]
fn an_allowance_can_name_the_pane_and_the_keys() {
    let takes = Takes::prompts_and_metadata().input(|attempt| {
        attempt.pane.as_str() == "pane-author"
            && matches!(&attempt.input, SentInput::Keys { keys, .. }
                if keys.iter().all(|key| *key == Key::CtrlU))
    });
    let space = space(takes);
    assert_eq!(
        delete_three_lines(&space, "pane-author"),
        InputOutcome::Delivered
    );
}
