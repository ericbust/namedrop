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
        relayed: false,
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
    fn relayed(facts: DeliveryFacts<'_>) -> DeliveryFacts<'_> {
        DeliveryFacts {
            relayed: true,
            ..facts
        }
    }
    for kind in [
        DeliveryKind::Settle,
        DeliveryKind::Channel,
        DeliveryKind::DirectMessage,
    ] {
        assert_eq!(
            plan(&relayed(answers_own(facts(kind, Some(&reply))))),
            Plan::Armed(Armed::NoReply(NoReplyCase::Relayed)),
            "{kind:?}"
        );
        assert_eq!(
            plan(&DeliveryFacts {
                leftover: true,
                ..relayed(facts(kind, Some(&reply)))
            }),
            Plan::Explicit(Explicit::LeftoverInput)
        );
    }
    assert_eq!(
        plan(&relayed(facts(DeliveryKind::Settle, Some(&unbound)))),
        Plan::Explicit(Explicit::NoBoundLine)
    );
    for kind in [DeliveryKind::ContextRequest, DeliveryKind::Nudge] {
        assert_eq!(
            plan(&relayed(facts(kind, Some(&reply)))),
            Plan::Explicit(Explicit::Kind(kind))
        );
    }
    assert_eq!(
        plan(&relayed(facts(DeliveryKind::ReportToHuman, None))),
        Plan::Armed(Armed::NoReply(NoReplyCase::ReportToHuman))
    );
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
        reply_line: Some("reply: namedrop reply --input-json '{}'".into()),
        no_reply_line: Some("complete.no-reply: namedrop complete --no-reply".into()),
        failure: None,
        awaits: Vec::new(),
        first_armed_at_ms: None,
        promised_turn: None,
    }
}

fn stop(prompt: &str, text: &str) -> StopFacts {
    StopFacts {
        session: "session".into(),
        turn: Some(turn(prompt)),
        running_tasks: Vec::new(),
        started_tasks: Vec::new(),
        notified_tasks: Vec::new(),
        text: FinalText::bounded(text),
    }
}

/// A Stop of `prompt` whose turn started `task`, still running.
fn started(prompt: &str, task: &str, text: &str) -> StopFacts {
    StopFacts {
        running_tasks: vec![task.to_owned()],
        started_tasks: vec![task.to_owned()],
        ..stop(prompt, text)
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
    let background = started("prompt-1", "task-1", "Working on it.");
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
                disposition: Some(crate::DeliveryDisposition::NoReply),
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
            stop("prompt-1", "I asked @claude; the answer follows."),
            SettleFacts {
                waiting: true,
                ..none
            },
            Verdict::Leave(LeaveReason::Held),
        ),
        (
            &open,
            stop("prompt-1", "pong"),
            SettleFacts {
                disposition: Some(crate::DeliveryDisposition::NoReply),
                waiting: true,
                ..none
            },
            Verdict::Superseded,
        ),
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
            replied("@codex \\@claude can you check this?"),
        ),
        (
            &open,
            stop("prompt-1", "@codex pong"),
            none,
            replied("@codex \\@codex pong"),
        ),
        (
            &open,
            stop("prompt-1", "@codex @claude,\n@qa: pong"),
            none,
            replied("@codex \\@codex \\@claude,\n\\@qa: pong"),
        ),
        (
            &open,
            stop("prompt-1", "@claude @codex, pong"),
            none,
            replied("@codex \\@claude \\@codex, pong"),
        ),
        (
            &open,
            stop("prompt-1", "@codex"),
            none,
            replied("@codex \\@codex"),
        ),
        (
            &open,
            stop("prompt-1", "@codex, @claude"),
            none,
            replied("@codex \\@codex, \\@claude"),
        ),
        (
            &open,
            stop("prompt-1", "\\@claude and `@qa` agree, as does @reviewer."),
            none,
            replied("@codex \\@claude and `@qa` agree, as does @reviewer."),
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
    let relayed = claim(Armed::NoReply(NoReplyCase::Relayed), ClaimState::Open);
    for (stop, settle, expected) in [
        (
            stop("prompt-1", "  Option B.\n"),
            none,
            Verdict::Settle(Settlement::Relay("Option B.".into())),
        ),
        (
            stop("prompt-1", "Option B."),
            SettleFacts {
                disposition: Some(crate::DeliveryDisposition::NoReply),
                ..none
            },
            Verdict::Superseded,
        ),
        (
            stop("prompt-1", "Option B."),
            SettleFacts {
                explicit_pending: true,
                ..none
            },
            Verdict::Superseded,
        ),
        (
            stop("prompt-1", "Option B."),
            SettleFacts {
                disposition: Some(crate::DeliveryDisposition::Reply),
                ..none
            },
            Verdict::Superseded,
        ),
        (
            stop("prompt-1", " "),
            none,
            Verdict::Leave(LeaveReason::NoFinalText),
        ),
        (
            started("prompt-1", "task-1", "Research is running."),
            none,
            Verdict::Leave(LeaveReason::Held),
        ),
        (
            stop("prompt-1", "Research is running."),
            SettleFacts {
                waiting: true,
                ..none
            },
            Verdict::Leave(LeaveReason::Held),
        ),
        (
            stop("prompt-2", "Option B."),
            none,
            Verdict::Leave(LeaveReason::Interrupted),
        ),
    ] {
        assert_eq!(decide(&relayed, &stop, &settle), expected, "{settle:?}");
    }
    let waiting = SettleFacts {
        waiting: true,
        ..none
    };
    for case in [
        NoReplyCase::ReportToHuman,
        NoReplyCase::AnswersOwn,
        NoReplyCase::ResultSet,
    ] {
        let no_reply = claim(Armed::NoReply(case), ClaimState::Open);
        for (stop, settle) in [
            (stop("prompt-1", ""), none),
            (started("prompt-1", "task-1", "@claude still going"), none),
            (stop("prompt-1", "Asked @claude."), waiting),
        ] {
            assert_eq!(
                decide(&no_reply, &stop, &settle),
                Verdict::Settle(Settlement::NoReply),
                "{case:?}"
            );
        }
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
    observe_prompt(
        &state.0,
        "socket",
        "session",
        &turn(prompt),
        arm,
        PromptFacts {
            notified: &[],
            delivery: false,
        },
        10,
    )
    .armed
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
    assert!(stop_has_work(&state.0, "socket", "session", None, |_| None));
    assert!(!stop_has_work(
        &state.0,
        "socket",
        "other-session",
        None,
        |_| None
    ));
    assert!(!observe(&state, "prompt-2", None));
    assert_eq!(
        state.claims()[0].state,
        ClaimState::Left(LeaveReason::Interrupted)
    );
    assert!(!stop_has_work(&state.0, "socket", "session", None, |_| {
        None
    }));
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
        (STOP_NO_REPLY, &json!({"attempt_id":"attempt-1"}))
    );
    assert_eq!(
        state.claims()[0].state,
        ClaimState::Left(LeaveReason::OperationFailed)
    );
}

#[test]
fn a_failed_settle_keeps_its_error_when_the_warning_cannot_be_written() {
    for plan in [reply_plan(), Armed::NoReply(NoReplyCase::Relayed)] {
        let state = StateDirectory::new();
        observe(
            &state,
            "prompt-1",
            Some(armed("attempt-1", "prompt-1", plan)),
        );
        std::fs::write(state.0.join("namedrop.sqlite3"), "not a database").unwrap();
        let log = std::cell::RefCell::new(Vec::new());
        settle(
            &state,
            &stop("prompt-1", "pong"),
            &mut ran(&log, Err("refused".into())),
        );
        let claim = &state.claims()[0];
        assert_eq!(claim.state, ClaimState::Left(LeaveReason::OperationFailed));
        assert!(claim.failure.is_some());
    }
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
    let background = started(
        "prompt-1",
        "task-1",
        "Started the build; I will report when it ends.",
    );
    settle(&state, &background, &mut ran(&log, Ok(json!({}))));
    assert_eq!(state.claims()[0].state, ClaimState::Left(LeaveReason::Held));
    observe(&state, "continuation", None);
    settle(
        &state,
        &stop("continuation", "The build passed."),
        &mut ran(&log, Ok(json!({}))),
    );
    assert!(log.into_inner().is_empty(), "a held reply was sent");
    assert_eq!(
        observe_prompt(
            &state.0,
            "socket",
            "session",
            &turn("later"),
            None,
            PromptFacts {
                notified: &[],
                delivery: false
            },
            30
        )
        .left_open,
        [WayBack {
            attempt_id: crate::DeliveryAttemptId::new("attempt-1"),
            line: "reply: namedrop reply --input-json '{}'".to_owned(),
            answers: true,
            failure: None,
        }]
    );
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
            PromptFacts {
                notified: &[],
                delivery: false,
            },
            now_ms,
        );
        state.claims().len()
    };
    assert_eq!(later(19 + KEEP_MS), 1);
    assert_eq!(later(20 + KEEP_MS), 0);
}

#[test]
fn a_prompt_queued_behind_a_stop_leaves_the_claim_to_that_stops_worker() {
    let state = StateDirectory::new();
    observe(
        &state,
        "prompt-1",
        Some(armed("attempt-1", "prompt-1", reply_plan())),
    );
    mark_stopped(&state.0, "socket", "session", &turn("prompt-1"), 10);
    assert!(
        observe(
            &state,
            "prompt-2",
            Some(armed("attempt-2", "prompt-2", reply_plan()))
        ),
        "a delivery queued behind a Stop lost its claim"
    );
    settle(&state, &stop("prompt-2", "Second answer."), &mut |_| {
        Ok(json!({}))
    });
    let log = std::cell::RefCell::new(Vec::new());
    settle(
        &state,
        &stop("prompt-1", "First answer."),
        &mut ran(&log, Ok(json!({}))),
    );
    let log = log.into_inner();
    assert_eq!(log.len(), 1, "the stopped turn's answer was lost");
    assert_eq!(log[0].input["source"], "@codex First answer.");
    assert_eq!(
        state
            .claims()
            .iter()
            .map(|claim| claim.state)
            .collect::<Vec<_>>(),
        [
            ClaimState::Done(Done::Replied),
            ClaimState::Done(Done::Replied)
        ]
    );
    assert!(
        fs::read_dir(session_directory(&state.0, "socket", "session").join("stops"))
            .unwrap()
            .next()
            .is_none(),
        "a settled turn kept its Stop marker"
    );

    let state = StateDirectory::new();
    observe(
        &state,
        "prompt-1",
        Some(armed("attempt-1", "prompt-1", reply_plan())),
    );
    mark_stopped(&state.0, "socket", "session", &turn("prompt-1"), 10);
    observe_prompt(
        &state.0,
        "socket",
        "session",
        &turn("prompt-2"),
        None,
        PromptFacts {
            notified: &[],
            delivery: false,
        },
        10 + STOPPED_GRACE_MS,
    );
    assert_eq!(
        state.claims()[0].state,
        ClaimState::Left(LeaveReason::OperationFailed),
        "a claim whose worker died stayed open"
    );
}

#[test]
fn a_claim_left_open_stays_while_later_prompts_show_it() {
    let state = StateDirectory::new();
    observe(
        &state,
        "prompt-1",
        Some(armed("attempt-1", "prompt-1", reply_plan())),
    );
    let shown = |prompt: &str, now_ms| {
        observe_prompt(
            &state.0,
            "socket",
            "session",
            &turn(prompt),
            None,
            PromptFacts {
                notified: &[],
                delivery: false,
            },
            now_ms,
        )
        .left_open
        .len()
    };
    assert_eq!(shown("prompt-2", 20), 1);
    assert_eq!(shown("prompt-3", 19 + KEEP_MS), 1);
    assert_eq!(shown("prompt-4", 18 + 2 * KEEP_MS), 1);
    assert_eq!(
        state.claims().len(),
        1,
        "a claim the agent was still being shown was pruned"
    );
}

#[test]
fn a_late_stop_worker_leaves_the_next_turns_claim_alone() {
    let state = StateDirectory::new();
    let at = |attempt: &str, prompt: &str, armed_at_ms| Claim {
        armed_at_ms,
        updated_at_ms: armed_at_ms,
        ..armed(attempt, prompt, reply_plan())
    };
    let prompt = |prompt: &str, arm: Claim, now_ms| {
        observe_prompt(
            &state.0,
            "socket",
            "session",
            &turn(prompt),
            Some(arm),
            PromptFacts {
                notified: &[],
                delivery: false,
            },
            now_ms,
        )
        .armed
    };
    assert!(prompt("prompt-1", at("attempt-1", "prompt-1", 10), 10));
    mark_stopped(&state.0, "socket", "session", &turn("prompt-1"), 12);
    assert!(prompt("prompt-2", at("attempt-2", "prompt-2", 15), 15));
    let log = std::cell::RefCell::new(Vec::new());
    settle_at_stop(
        &state.0,
        "socket",
        &stop("prompt-1", "First answer."),
        12,
        &mut ran(&log, Ok(json!({}))),
    );
    assert_eq!(
        state
            .claims()
            .iter()
            .map(|claim| claim.state)
            .collect::<Vec<_>>(),
        [ClaimState::Done(Done::Replied), ClaimState::Open],
        "a late worker ended the next turn's claim"
    );
    assert!(stop_has_work(&state.0, "socket", "session", None, |_| None));
    settle_at_stop(
        &state.0,
        "socket",
        &stop("prompt-2", "Second answer."),
        20,
        &mut ran(&log, Ok(json!({}))),
    );
    let sources: Vec<_> = log
        .into_inner()
        .iter()
        .map(|operation| operation.input["source"].clone())
        .collect();
    assert_eq!(
        sources,
        [
            json!("@codex First answer."),
            json!("@codex Second answer.")
        ]
    );
}

#[test]
fn pruning_stop_markers_spares_one_still_being_written() {
    let state = StateDirectory::new();
    let stops = session_directory(&state.0, "socket", "session").join("stops");
    mark_stopped(&state.0, "socket", "session", &turn("prompt-3"), 30);
    fs::write(stops.join("in-flight.next"), "30").unwrap();
    observe_prompt(
        &state.0,
        "socket",
        "session",
        &turn("prompt-4"),
        None,
        PromptFacts {
            notified: &[],
            delivery: false,
        },
        30,
    );
    assert!(
        stops.join("in-flight.next").exists(),
        "a marker still being written was deleted"
    );
}

#[test]
fn only_running_work_the_claims_own_turn_started_holds_its_answer() {
    let none = SettleFacts::default();
    let relayed = claim(Armed::NoReply(NoReplyCase::Relayed), ClaimState::Open);
    let earlier_work = StopFacts {
        running_tasks: vec!["monitor-from-yesterday".into(), "dream-1".into()],
        ..stop("prompt-1", "Hi, doing well.")
    };
    assert_eq!(
        decide(&relayed, &earlier_work, &none),
        Verdict::Settle(Settlement::Relay("Hi, doing well.".into()))
    );
    assert_eq!(
        decide(&claim(reply_plan(), ClaimState::Open), &earlier_work, &none),
        replied("@codex Hi, doing well.")
    );
    let ended = StopFacts {
        started_tasks: vec!["task-1".into()],
        ..stop("prompt-1", "Done already.")
    };
    assert_eq!(
        decide(&relayed, &ended, &none),
        Verdict::Settle(Settlement::Relay("Done already.".into()))
    );
    let woken = Claim {
        awaits: vec!["task-1".into(), "task-2".into()],
        ..claim(Armed::NoReply(NoReplyCase::Relayed), ClaimState::Open)
    };
    let one_left = StopFacts {
        running_tasks: vec!["task-2".into(), "monitor-from-yesterday".into()],
        ..stop("prompt-1", "One of two finished.")
    };
    assert_eq!(
        decide(&woken, &one_left, &none),
        Verdict::Leave(LeaveReason::Held)
    );
    assert_eq!(
        decide(&woken, &stop("prompt-1", "Both finished."), &none),
        Verdict::Settle(Settlement::Relay("Both finished.".into()))
    );
}

fn notified(prompt: &str, task: &str, text: &str) -> StopFacts {
    StopFacts {
        notified_tasks: vec![task.to_owned()],
        ..stop(prompt, text)
    }
}

#[test]
fn a_held_answer_settles_at_the_stop_of_the_turn_its_works_notice_reached() {
    let state = StateDirectory::new();
    observe(
        &state,
        "prompt-1",
        Some(armed("attempt-1", "prompt-1", reply_plan())),
    );
    let log = std::cell::RefCell::new(Vec::new());
    settle(
        &state,
        &started("prompt-1", "build-1", "Started the build."),
        &mut ran(&log, Ok(json!({}))),
    );
    let held = &state.claims()[0];
    assert_eq!(held.state, ClaimState::Left(LeaveReason::Held));
    assert_eq!(held.awaits, ["build-1"]);
    assert!(
        stop_has_work(&state.0, "socket", "session", None, |_| None),
        "a Stop of the session would start no worker to wake it"
    );
    let other = observe_prompt(
        &state.0,
        "socket",
        "session",
        &turn("monitor-event"),
        None,
        PromptFacts {
            notified: &["monitor-from-yesterday".to_owned()],
            delivery: false,
        },
        30,
    );
    assert_eq!(
        other.left_open.len(),
        1,
        "another task's notice hid the way back"
    );
    let woken = observe_prompt(
        &state.0,
        "socket",
        "session",
        &turn("build-ended"),
        None,
        PromptFacts {
            notified: &["build-1".to_owned()],
            delivery: false,
        },
        40,
    );
    assert!(
        woken.left_open.is_empty(),
        "the turn that answers the delivery also showed its way back"
    );
    settle(
        &state,
        &notified("build-ended", "build-1", "The build passed."),
        &mut ran(&log, Ok(json!({}))),
    );
    let log = log.into_inner();
    assert_eq!(log.len(), 1, "the woken claim did not settle once");
    assert_eq!(log[0].input["source"], "@codex The build passed.");
    let claim = &state.claims()[0];
    assert_eq!(
        (claim.state, &claim.turn, claim.first_armed_at_ms),
        (
            ClaimState::Done(Done::Replied),
            &turn("build-ended"),
            Some(1)
        ),
        "a helper asked before the wake no longer kept the answer back"
    );
    assert_eq!(
        claim.armed_at_ms, 40,
        "the relay would look for an answer posted this turn from the Stop on, not from the turn's start"
    );
}

#[test]
fn one_notice_wakes_only_the_oldest_claim_held_on_its_task() {
    let state = StateDirectory::new();
    let log = std::cell::RefCell::new(Vec::new());
    for (attempt, prompt, armed_at_ms) in
        [("attempt-1", "prompt-1", 1), ("attempt-2", "prompt-2", 5)]
    {
        observe(
            &state,
            prompt,
            Some(Claim {
                armed_at_ms,
                ..armed(attempt, prompt, reply_plan())
            }),
        );
        settle(
            &state,
            &started(prompt, "agent-1", "Asked the agent."),
            &mut ran(&log, Ok(json!({}))),
        );
    }
    settle(
        &state,
        &notified("agent-done", "agent-1", "The agent reported."),
        &mut ran(&log, Ok(json!({}))),
    );
    let log = log.into_inner();
    assert_eq!(log.len(), 1, "one final message went to two deliveries");
    assert_eq!(log[0].input["attempt_id"], "attempt-1");
    let states: Vec<_> = state.claims().iter().map(|claim| claim.state).collect();
    assert_eq!(
        states,
        [
            ClaimState::Done(Done::Replied),
            ClaimState::Left(LeaveReason::Held)
        ]
    );
}

const RECORDED: &str =
    include_str!("../tests/fixtures/final-reply/claude-2.1.319-background-turns.jsonl");

#[test]
fn the_stop_reads_its_own_turn_of_a_recorded_transcript() {
    let read = |prompt: &str| turn_tasks(RECORDED.as_bytes(), true, prompt);
    let tasks = |ids: &[&str]| ids.iter().map(|id| (*id).to_owned()).collect::<Vec<_>>();
    assert_eq!(
        read("prompt-start"),
        TurnTasks {
            started: Some(tasks(&["task-first"])),
            notified: Vec::new()
        }
    );
    assert_eq!(
        read("prompt-busy"),
        TurnTasks {
            started: Some(Vec::new()),
            notified: tasks(&["task-first"])
        },
        "the notice Claude Code queued into the running turn was missed"
    );
    assert_eq!(
        read("prompt-wait"),
        TurnTasks {
            started: Some(tasks(&["task-second"])),
            notified: Vec::new()
        },
        "the queue's own log of a later notice counted"
    );
    assert_eq!(
        read("prompt-notice"),
        TurnTasks {
            started: Some(Vec::new()),
            notified: tasks(&["task-second"])
        }
    );
    assert_eq!(read("prompt-unknown").started, None);

    // A tail that starts inside the turn may have lost one of its starts.
    let lines: Vec<&str> = RECORDED.lines().collect();
    let first = lines
        .iter()
        .position(|line| line.contains(r#""promptId":"prompt-wait""#))
        .unwrap();
    let from_inside = lines[first + 1..].join("\n");
    assert_eq!(
        turn_tasks(from_inside.as_bytes(), false, "prompt-wait").started,
        None
    );
    let from_before = lines[first - 1..].join("\n");
    assert_eq!(
        turn_tasks(from_before.as_bytes(), false, "prompt-wait").started,
        None,
        "a tail with no earlier turn's record cannot show where this turn began"
    );
    let with_earlier = lines[first - 8..].join("\n");
    assert!(
        lines[first - 8..first]
            .iter()
            .any(|line| line.contains(r#""promptId":"prompt-busy""#))
    );
    assert_eq!(
        turn_tasks(with_earlier.as_bytes(), false, "prompt-wait").started,
        Some(tasks(&["task-second"]))
    );

    // From a real SendMessage that resumed a finished agent.
    let resumed = json!({"type":"user","promptId":"prompt-resume","toolUseResult":
        {"success":true,"message":"Resuming agent a56e6ad","resumedAgentId":"agent-resumed"}});
    assert_eq!(
        turn_tasks(resumed.to_string().as_bytes(), true, "prompt-resume").started,
        Some(tasks(&["agent-resumed"]))
    );
}

#[test]
fn a_notice_is_about_the_task_its_opening_id_names() {
    let recorded = RECORDED
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find(|record| record["promptId"] == "prompt-notice")
        .unwrap();
    let text = recorded["message"]["content"].as_str().unwrap();
    assert_eq!(notice_task(text), Some("task-second"));
    let quoting = text.replace(
        "</task-notification>",
        "<result>It waited on <task-id>task-first</task-id>.</result>\n</task-notification>",
    );
    assert_eq!(notice_task(&quoting), Some("task-second"));
    assert_eq!(notice_task(&format!("\n{text}")), Some("task-second"));
    assert_eq!(
        notice_task("Please check <task-id>task-first</task-id>"),
        None
    );
    assert_eq!(
        notice_task("<task-notification>\n<summary><task-id>task-first</task-id></summary>"),
        None
    );
}

#[test]
fn a_turn_split_by_another_turns_records_keeps_its_second_half() {
    let lines: Vec<&str> = RECORDED.lines().collect();
    let wait: Vec<usize> = (0..lines.len())
        .filter(|&index| lines[index].contains(r#""promptId":"prompt-wait""#))
        .collect();
    let busy: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|line| {
            line.contains(r#""promptId":"prompt-busy""#) || line.contains("queued_command")
        })
        .collect();
    // The wait turn's prompt, then the busy turn's records with its queued
    // notice, then the rest of the wait turn: its start, and a queued notice.
    let queued = lines
        .iter()
        .find(|line| line.contains("queued_command"))
        .unwrap();
    let mut split = lines[..=wait[0]].to_vec();
    split.extend(&busy);
    split.extend(&lines[wait[0] + 1..=wait[wait.len() - 1]]);
    let second_half = queued.replace("task-first", "task-third");
    split.push(&second_half);
    let read = turn_tasks(split.join("\n").as_bytes(), true, "prompt-wait");
    assert_eq!(read.started, Some(vec!["task-second".to_owned()]));
    assert_eq!(
        read.notified,
        ["task-third"],
        "the other turn's notice counted, or ours was lost"
    );
}

#[test]
fn a_wake_with_no_record_of_its_turn_looks_for_an_earlier_answer_from_the_stop_only() {
    let state = StateDirectory::new();
    observe(
        &state,
        "prompt-1",
        Some(armed("attempt-1", "prompt-1", reply_plan())),
    );
    let log = std::cell::RefCell::new(Vec::new());
    settle(
        &state,
        &started("prompt-1", "build-1", "Started the build."),
        &mut ran(&log, Ok(json!({}))),
    );
    // No prompt hook recorded the notice turn, as when it missed the lock.
    settle(
        &state,
        &notified("build-ended", "build-1", "The build passed."),
        &mut ran(&log, Ok(json!({}))),
    );
    let claim = &state.claims()[0];
    assert_eq!(claim.state, ClaimState::Done(Done::Replied));
    assert_eq!(
        claim.armed_at_ms, 20,
        "the relay would take any post since the first turn as this answer"
    );
}

#[test]
fn only_a_few_recent_turns_are_recorded_and_none_once_nothing_is_live() {
    let state = StateDirectory::new();
    observe(
        &state,
        "prompt-1",
        Some(armed("attempt-1", "prompt-1", reply_plan())),
    );
    let log = std::cell::RefCell::new(Vec::new());
    settle(
        &state,
        &started("prompt-1", "build-1", "Started the build."),
        &mut ran(&log, Ok(json!({}))),
    );
    let turns = || {
        fs::read_dir(session_directory(&state.0, "socket", "session").join("turns"))
            .map_or(0, |entries| entries.count())
    };
    for index in 0..20 {
        observe_prompt(
            &state.0,
            "socket",
            "session",
            &turn(&format!("typed-{index}")),
            None,
            PromptFacts::default(),
            30 + index,
        );
    }
    assert!(turns() <= RECENT_TURNS, "{} turn records kept", turns());
    settle(
        &state,
        &notified("build-ended", "build-1", "The build passed."),
        &mut ran(&log, Ok(json!({}))),
    );
    observe_prompt(
        &state.0,
        "socket",
        "session",
        &turn("after"),
        None,
        PromptFacts::default(),
        60,
    );
    assert_eq!(turns(), 0, "turn records outlived every live claim");
}

#[test]
fn the_stop_hook_finds_a_held_tasks_notice_or_says_it_cannot_tell() {
    let state = StateDirectory::new();
    let path = state.0.join("transcript.jsonl");
    let tasks = vec!["task-first".to_owned()];
    let notice = RECORDED
        .lines()
        .find(|line| line.contains("queued_command"))
        .unwrap();
    // The notice straddles the first chunk boundary from the end.
    let mut bytes = vec![b' '; 3 * 1024 * 1024];
    let at = bytes.len() - 1024 * 1024 - notice.len() / 2;
    bytes[at..at + notice.len()].copy_from_slice(notice.as_bytes());
    fs::write(&path, &bytes).unwrap();
    let scan = |limit, budget| names_notice(&path, &tasks, limit, budget);
    assert_eq!(scan(8 << 20, Duration::from_secs(5)), Some(true));
    assert_eq!(
        scan((1024 * 1024 - notice.len()) as u64, Duration::from_secs(5)),
        Some(false)
    );
    assert_eq!(
        names_notice(
            &path,
            &["task-other".to_owned()],
            8 << 20,
            Duration::from_secs(5)
        ),
        Some(false)
    );
    assert_eq!(
        scan(8 << 20, Duration::ZERO),
        None,
        "a scan out of time claimed an answer"
    );
    assert_eq!(
        names_notice(
            &state.0.join("missing"),
            &tasks,
            8 << 20,
            Duration::from_secs(5)
        ),
        None
    );
}
