use super::*;
use crate::mux::{
    Admit, Badge, Budget, InputKind, InputOutcome, Key, MuxAct, MuxConnector, MuxRead, PaneId,
    PromptRequest, Recipient, Scope, ScreenRequest, Style, Wrap,
};
use std::{path::Path, time::Duration};

fn author() -> FakeMux {
    FakeMux::world()
        .endpoint("socket-a")
        .space("workspace-1")
        .checkout("/repo")
        .pane("pane-author")
        .claude()
        .session("session-author")
        .idle()
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

fn prompt(space: &StrictSpace, pane: &str) -> InputOutcome {
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
            admit: Admit::Settled,
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

#[test]
fn a_space_that_sees_only_what_it_takes_drops_quietly() {
    let space = space(Takes::prompts_and_metadata());
    space.set_screen("pane-author", "ready");
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
    space
        .apply(&Effect::Badges {
            pane: PaneId::new("pane-author"),
            badges: vec![Badge::Awaiting(1)],
        })
        .unwrap();
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
    assert!(
        space
            .apply(&Effect::Badges {
                pane: PaneId::new("pane-author"),
                badges: vec![Badge::Awaiting(1)],
            })
            .is_err()
    );
}

#[test]
#[should_panic(expected = "the strict space saw 3 violation(s)")]
fn every_violation_is_reported() {
    let space = space(Takes::nothing());
    prompt(&space, "pane-author");
    delete_three_lines(&space, "pane-author");
    space.apply(&notify()).unwrap();
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
#[should_panic(expected = "the test's own failure")]
fn a_failing_test_keeps_its_own_message_when_the_space_also_fails() {
    let space = space(Takes::nothing());
    space.apply(&notify()).unwrap();
    panic!("the test's own failure");
}

#[test]
fn an_allowance_can_name_the_pane_and_the_keys() {
    let takes = Takes::prompts_and_metadata().input(|attempt| {
        attempt.pane.as_str() == "pane-author"
            && matches!(&attempt.input, super::super::SentInput::Keys { keys, .. }
                if keys.iter().all(|key| *key == Key::CtrlU))
    });
    let space = space(takes);
    assert_eq!(
        delete_three_lines(&space, "pane-author"),
        InputOutcome::Delivered
    );
}
