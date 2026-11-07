use super::*;
use serde_json::json;
use std::path::{Path, PathBuf};

fn reply_template() -> Value {
    json!({"request_id":"reply-attempt-1","source":"@codex <reply body>",
           "in_reply_to":"message-1","author_kind":"agent_session","attempt_id":"attempt-1"})
}

fn tracked_template() -> Value {
    let mut template = reply_template();
    template["work_outcome"] = json!("needs_continuation");
    template
}

fn facts<'a>(kind: DeliveryKind, reply: Option<&'a Value>) -> DeliveryFacts<'a> {
    DeliveryFacts {
        kind,
        reply,
        no_reply: true,
        other_line: false,
        answers_own: false,
        leftover: false,
    }
}

#[test]
fn plan_follows_the_measured_rule_and_keeps_explicit_kinds_explicit() {
    let reply = reply_template();
    let tracked = tracked_template();
    let unbound = json!({"source":"@codex hello"});
    fn answers_own(facts: DeliveryFacts<'_>) -> DeliveryFacts<'_> {
        DeliveryFacts {
            answers_own: true,
            ..facts
        }
    }
    let cases = [
        (
            facts(DeliveryKind::Settle, Some(&reply)),
            Plan::Armed(Armed::Reply {
                template: reply.clone(),
                tracked: false,
            }),
        ),
        (
            answers_own(facts(DeliveryKind::Settle, Some(&reply))),
            Plan::Armed(Armed::NoReply(NoReplyCase::AnswersOwn)),
        ),
        (
            answers_own(facts(DeliveryKind::Settle, Some(&tracked))),
            Plan::Armed(Armed::Reply {
                template: tracked.clone(),
                tracked: true,
            }),
        ),
        (
            facts(DeliveryKind::ReportToHuman, None),
            Plan::Armed(Armed::NoReply(NoReplyCase::ReportToHuman)),
        ),
        (
            facts(DeliveryKind::ResultSet, Some(&reply)),
            Plan::Armed(Armed::NoReply(NoReplyCase::ResultSet)),
        ),
        (
            facts(DeliveryKind::Settle, Some(&unbound)),
            Plan::Explicit(Explicit::NoBoundLine),
        ),
        (
            facts(DeliveryKind::Settle, None),
            Plan::Explicit(Explicit::NoBoundLine),
        ),
        (
            DeliveryFacts {
                no_reply: false,
                ..facts(DeliveryKind::Settle, Some(&reply))
            },
            Plan::Explicit(Explicit::NoBoundLine),
        ),
        (
            DeliveryFacts {
                leftover: true,
                ..facts(DeliveryKind::ReportToHuman, None)
            },
            Plan::Explicit(Explicit::LeftoverInput),
        ),
        (
            DeliveryFacts {
                other_line: true,
                ..facts(DeliveryKind::Settle, Some(&reply))
            },
            Plan::Explicit(Explicit::OtherLine),
        ),
        (
            DeliveryFacts {
                other_line: true,
                ..answers_own(facts(DeliveryKind::Settle, Some(&reply)))
            },
            Plan::Explicit(Explicit::OtherLine),
        ),
        (
            DeliveryFacts {
                other_line: true,
                ..facts(DeliveryKind::ReportToHuman, None)
            },
            Plan::Explicit(Explicit::OtherLine),
        ),
    ];
    for (facts, expected) in cases {
        assert_eq!(plan(&facts), expected);
    }
    for kind in [
        DeliveryKind::ContextRequest,
        DeliveryKind::TrackedResultSet,
        DeliveryKind::StaleNudge,
        DeliveryKind::Nudge,
        DeliveryKind::DirectMessage,
        DeliveryKind::Channel,
    ] {
        assert_eq!(
            plan(&facts(kind, Some(&reply))),
            Plan::Explicit(Explicit::Kind(kind))
        );
    }
}

fn turn(id: &str) -> HostTurn {
    HostTurn::from_ids(Some(id), None).unwrap()
}

fn claim(plan: Armed, state: ClaimState) -> Claim {
    Claim {
        attempt_id: crate::DeliveryAttemptId::new("attempt-1"),
        namespace: crate::StateNamespace::for_space("socket", "workspace"),
        location: Location {
            state: "/state".into(),
            socket: "socket".into(),
            workspace: "workspace".into(),
            pane: "pane".into(),
        },
        turn: turn("prompt-1"),
        plan,
        state,
        armed_at_ms: 1,
        updated_at_ms: 1,
    }
}

fn stop(prompt: &str, text: &str) -> StopFacts {
    StopFacts {
        session: "session".into(),
        turn: Some(turn(prompt)),
        background_running: false,
        text: FinalText::bounded(text),
    }
}

fn reply_plan() -> Armed {
    Armed::Reply {
        template: reply_template(),
        tracked: false,
    }
}

fn replied(source: &str) -> Verdict {
    let mut input = reply_template();
    input["source"] = json!(source);
    Verdict::Settle(Settlement::Reply(input))
}

#[test]
fn a_stop_settles_only_its_own_open_claim_and_explicit_settles_win() {
    let open = claim(reply_plan(), ClaimState::Open);
    let none = SettleFacts::default();
    let background = StopFacts {
        background_running: true,
        ..stop("prompt-1", "Working on it.")
    };
    let cases = [
        (
            &open,
            stop("prompt-1", "pong"),
            none,
            replied("@codex pong"),
        ),
        (
            &open,
            stop("prompt-2", "pong"),
            none,
            Verdict::Leave(LeaveReason::Interrupted),
        ),
        (
            &open,
            StopFacts {
                turn: None,
                ..stop("prompt-1", "pong")
            },
            none,
            Verdict::Leave(LeaveReason::Interrupted),
        ),
        (
            &open,
            stop("prompt-1", "pong"),
            SettleFacts {
                disposed: true,
                ..none
            },
            Verdict::Superseded,
        ),
        (
            &open,
            stop("prompt-1", "pong"),
            SettleFacts {
                explicit_pending: true,
                ..none
            },
            Verdict::Superseded,
        ),
        (&open, background, none, Verdict::Leave(LeaveReason::Held)),
        (
            &open,
            stop("prompt-1", "  \n"),
            none,
            Verdict::Leave(LeaveReason::NoFinalText),
        ),
        (
            &open,
            stop("prompt-1", "@claude can you check this?"),
            none,
            Verdict::Leave(LeaveReason::AddressesOthers),
        ),
        (
            &open,
            stop("prompt-1", "@codex pong"),
            none,
            replied("@codex @codex pong"),
        ),
    ];
    for (claim, stop, settle, expected) in cases {
        assert_eq!(
            decide(claim, &stop, &settle),
            expected,
            "{}",
            stop.text.text
        );
    }
    for state in [
        ClaimState::Left(LeaveReason::Held),
        ClaimState::Done(Done::Replied),
        ClaimState::Left(LeaveReason::Interrupted),
    ] {
        let claim = claim(reply_plan(), state);
        assert_eq!(
            decide(&claim, &stop("prompt-1", "pong"), &none),
            Verdict::Ignore
        );
    }
    let no_reply = claim(Armed::NoReply(NoReplyCase::ReportToHuman), ClaimState::Open);
    for stop in [
        stop("prompt-1", ""),
        StopFacts {
            background_running: true,
            ..stop("prompt-1", "@claude still going")
        },
    ] {
        assert_eq!(
            decide(&no_reply, &stop, &none),
            Verdict::Settle(Settlement::NoReply)
        );
    }
}

#[test]
fn the_reply_grammar_keeps_the_addressee_and_drops_its_repeated_mention() {
    let parsed = crate::parse_mentions("@codex @codex pong").unwrap();
    assert_eq!(parsed.recipients.aliases, ["codex"]);
    assert_eq!(parsed.body, "pong");
}

#[test]
fn a_long_final_message_is_cut_with_a_visible_notice() {
    let long = "x".repeat(crate::TERMINAL_RELAY_MAX_CHARS + 5);
    let text = FinalText::bounded(&long);
    assert!(text.truncated);
    assert_eq!(text.text.chars().count(), crate::TERMINAL_RELAY_MAX_CHARS);
    let Verdict::Settle(Settlement::Reply(input)) = decide(
        &claim(reply_plan(), ClaimState::Open),
        &StopFacts {
            text,
            ..stop("prompt-1", "")
        },
        &SettleFacts::default(),
    ) else {
        panic!("a long message did not settle");
    };
    assert!(
        input["source"]
            .as_str()
            .unwrap()
            .ends_with("x\n\n[Cut at 16,000 characters by Namedrop.]")
    );
    assert!(!FinalText::bounded("pong").truncated);
}

#[test]
fn a_tracked_worker_states_its_outcome_on_one_strict_closing_line() {
    let cases = [
        (
            "Done.\nwork_outcome: completed",
            "Done.",
            TrackedOutcome::Completed,
        ),
        (
            "Half way.\n\nwork_outcome: needs_continuation\n",
            "Half way.",
            TrackedOutcome::NeedsContinuation,
        ),
        (
            "Stuck.\nwork_outcome: blocked: the build needs a token",
            "Stuck.",
            TrackedOutcome::Blocked {
                reason: "the build needs a token".into(),
            },
        ),
        (
            "Stuck.\nwork_outcome: blocked",
            "Stuck.\nwork_outcome: blocked",
            TrackedOutcome::NeedsContinuation,
        ),
        (
            "Stuck.\nwork_outcome: blocked:   ",
            "Stuck.\nwork_outcome: blocked:",
            TrackedOutcome::NeedsContinuation,
        ),
        (
            "work_outcome: completed\nThen more text.",
            "work_outcome: completed\nThen more text.",
            TrackedOutcome::NeedsContinuation,
        ),
        ("All done.", "All done.", TrackedOutcome::NeedsContinuation),
        ("work_outcome: completed", "", TrackedOutcome::Completed),
    ];
    for (text, body, outcome) in cases {
        assert_eq!(split_outcome_trailer(text), (body, outcome), "{text}");
    }
    let tracked = claim(
        Armed::Reply {
            template: tracked_template(),
            tracked: true,
        },
        ClaimState::Open,
    );
    let settled = |text: &str| decide(&tracked, &stop("prompt-1", text), &SettleFacts::default());
    let Verdict::Settle(Settlement::Reply(input)) =
        settled("Stuck.\nwork_outcome: blocked: no token")
    else {
        panic!("a tracked reply did not settle");
    };
    assert_eq!(input["source"], "@codex Stuck.");
    assert_eq!(input["work_outcome"], "blocked");
    assert_eq!(input["blocked_reason"], "no token");
    let Verdict::Settle(Settlement::Reply(input)) = settled("Partial result.") else {
        panic!("a tracked reply without a closing line did not settle");
    };
    assert_eq!(input["work_outcome"], "needs_continuation");
    assert!(input.get("blocked_reason").is_none());
    assert_eq!(
        settled("work_outcome: completed"),
        Verdict::Leave(LeaveReason::NoFinalText)
    );
}

#[test]
fn both_hooks_of_one_recorded_turn_name_the_same_host_turn() {
    for (prompt, stop) in [
        (
            include_str!("../tests/fixtures/final-reply/claude-2.1.319-user-prompt-submit.json"),
            include_str!(
                "../tests/fixtures/final-reply/claude-2.1.319-stop-background-running.json"
            ),
        ),
        (
            include_str!("../tests/fixtures/final-reply/codex-0.167.0-user-prompt-submit.json"),
            include_str!("../tests/fixtures/final-reply/codex-0.167.0-stop.json"),
        ),
    ] {
        let turn = |payload: &str| {
            let payload: Value = serde_json::from_str(payload).unwrap();
            HostTurn::from_ids(payload["prompt_id"].as_str(), payload["turn_id"].as_str()).unwrap()
        };
        assert_eq!(turn(prompt), turn(stop));
    }
    assert_eq!(HostTurn::from_ids(Some("a"), Some("b")), None);
    assert_eq!(HostTurn::from_ids(Some(" "), None), None);
    assert_ne!(
        HostTurn::from_ids(Some("same"), None),
        HostTurn::from_ids(None, Some("same"))
    );
}

struct StateDirectory(PathBuf);

impl StateDirectory {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "namedrop-final-reply-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn claims(&self) -> Vec<Claim> {
        read_claims(&session_directory(&self.0, "socket", "session"))
    }
}

impl Drop for StateDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn observe(state: &StateDirectory, prompt: &str, arm: Option<Claim>) -> bool {
    observe_prompt(&state.0, "socket", "session", &turn(prompt), arm, 10)
}

fn armed(attempt: &str, prompt: &str, plan: Armed) -> Claim {
    Claim {
        attempt_id: crate::DeliveryAttemptId::new(attempt),
        turn: turn(prompt),
        ..claim(plan, ClaimState::Open)
    }
}

#[test]
fn a_session_holds_one_live_claim_and_later_prompts_end_the_ones_that_lost_their_turn() {
    let state = StateDirectory::new();
    assert!(!observe(&state, "prompt-0", None));
    assert!(
        !session_directory(&state.0, "socket", "session").exists(),
        "an ordinary prompt with no claims wrote state"
    );
    assert!(observe(
        &state,
        "prompt-1",
        Some(armed("attempt-1", "prompt-1", reply_plan()))
    ));
    assert!(
        !observe(
            &state,
            "prompt-1",
            Some(armed("attempt-2", "prompt-1", reply_plan()))
        ),
        "a second live claim was armed in one session"
    );
    assert_eq!(state.claims().len(), 1);
    assert!(has_live_claims(&state.0, "socket", "session"));
    assert!(!has_live_claims(&state.0, "socket", "other-session"));
    assert!(!observe(&state, "prompt-2", None));
    assert_eq!(
        state.claims()[0].state,
        ClaimState::Left(LeaveReason::Interrupted)
    );
    assert!(!has_live_claims(&state.0, "socket", "session"));
    assert!(observe(
        &state,
        "prompt-3",
        Some(armed("attempt-1", "prompt-3", reply_plan()))
    ));
    let claims = state.claims();
    assert_eq!(claims.len(), 1, "a re-armed attempt kept its old claim");
    assert_eq!(
        (claims[0].state, &claims[0].turn),
        (ClaimState::Open, &turn("prompt-3"))
    );
}

fn ran(
    log: &std::cell::RefCell<Vec<BoundOperation>>,
    result: Result<Value, String>,
) -> impl FnMut(BoundOperation) -> Result<Value, String> + '_ {
    move |operation| {
        log.borrow_mut().push(operation);
        result.clone()
    }
}

fn settle(
    state: &StateDirectory,
    stop: &StopFacts,
    run: &mut dyn FnMut(BoundOperation) -> Result<Value, String>,
) {
    settle_at_stop(&state.0, "socket", stop, 20, run);
}

#[test]
fn the_stop_runs_the_bound_operation_once_and_records_how_the_claim_ended() {
    let state = StateDirectory::new();
    observe(
        &state,
        "prompt-1",
        Some(armed("attempt-1", "prompt-1", reply_plan())),
    );
    let log = std::cell::RefCell::new(Vec::new());
    settle(
        &state,
        &stop("prompt-1", "pong"),
        &mut ran(&log, Ok(json!({}))),
    );
    settle(
        &state,
        &stop("prompt-1", "pong"),
        &mut ran(&log, Ok(json!({}))),
    );
    let log = log.into_inner();
    assert_eq!(log.len(), 1, "a repeated Stop settled twice");
    assert_eq!(log[0].operation, "reply");
    assert_eq!(log[0].input["source"], "@codex pong");
    assert_eq!(
        (
            log[0].state.as_path(),
            log[0].socket.as_str(),
            log[0].workspace.as_str(),
            log[0].pane.as_str()
        ),
        (Path::new("/state"), "socket", "workspace", "pane")
    );
    assert_eq!(state.claims()[0].state, ClaimState::Done(Done::Replied));

    let state = StateDirectory::new();
    observe(
        &state,
        "prompt-1",
        Some(armed(
            "attempt-1",
            "prompt-1",
            Armed::NoReply(NoReplyCase::AnswersOwn),
        )),
    );
    let log = std::cell::RefCell::new(Vec::new());
    settle(
        &state,
        &stop("prompt-1", "Thanks, noted."),
        &mut ran(&log, Err("refused".into())),
    );
    let log = log.into_inner();
    assert_eq!(
        (log[0].operation, &log[0].input),
        ("complete", &json!({"attempt_id":"attempt-1"}))
    );
    assert_eq!(
        state.claims()[0].state,
        ClaimState::Left(LeaveReason::OperationFailed)
    );
}

#[test]
fn background_work_at_the_stop_leaves_a_reply_for_the_agent_to_settle() {
    let state = StateDirectory::new();
    observe(
        &state,
        "prompt-1",
        Some(armed("attempt-1", "prompt-1", reply_plan())),
    );
    let log = std::cell::RefCell::new(Vec::new());
    let background = StopFacts {
        background_running: true,
        ..stop("prompt-1", "Started the build; I will report when it ends.")
    };
    settle(&state, &background, &mut ran(&log, Ok(json!({}))));
    assert_eq!(state.claims()[0].state, ClaimState::Left(LeaveReason::Held));
    observe(&state, "continuation", None);
    settle(
        &state,
        &stop("continuation", "The build passed."),
        &mut ran(&log, Ok(json!({}))),
    );
    assert!(log.into_inner().is_empty(), "a held reply was sent");
    assert_eq!(state.claims()[0].state, ClaimState::Left(LeaveReason::Held));
    assert!(observe(
        &state,
        "next",
        Some(armed("attempt-2", "next", reply_plan()))
    ));
}

#[test]
fn finished_claims_are_pruned_after_a_day() {
    let state = StateDirectory::new();
    observe(
        &state,
        "prompt-1",
        Some(armed("attempt-1", "prompt-1", reply_plan())),
    );
    settle(&state, &stop("prompt-1", "pong"), &mut |_| Ok(json!({})));
    let later = |now_ms| {
        observe_prompt(
            &state.0,
            "socket",
            "session",
            &turn("prompt-2"),
            None,
            now_ms,
        );
        state.claims().len()
    };
    assert_eq!(later(19 + KEEP_MS), 1);
    assert_eq!(later(20 + KEEP_MS), 0);
}
