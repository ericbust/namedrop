use super::{key_payload, one_line};
use crate::{
    AgentRuntime, AgentSessionId,
    mux::{
        AgentIdentity, AgentKind, Applied, Badge, Effect, InputKind, InputOutcome, Key, MuxAct,
        PaneId, PromptRequest, Recipient, Retry, SessionClaim, StateTexts, TabId,
        processes::LocalRead,
        rex::{
            RexMux,
            control::Fault,
            tests::{
                Events, Local, Request, Script, State, binding, fixture, one_block, rex_error,
                rex_shared,
            },
            wire::TERMINAL,
        },
    },
};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

/// A root status change for `block:b`, as Rex sends it on an attached
/// connection.
fn status(state: &str) -> Value {
    json!({"type": "session_event", "payload": {"session_id": "session:s", "event": {
        "type": "block_event", "payload": {"block_id": "block:b", "creator_name": TERMINAL,
        "name": "program_status_changed", "payload": {"id": "", "reason": "report",
        "record": {"state": state, "app": "claude-code", "updated_at": "2026-11-13T00:00:00Z",
                   "owner": {"pid": 75361, "name": "Python"}}}}}}})
}

fn block_event(name: &str, payload: Value) -> Value {
    json!({"type": "session_event", "payload": {"session_id": "session:s", "event": {
        "type": "block_event", "payload": {"block_id": "block:b", "creator_name": TERMINAL,
        "name": name, "payload": payload}}}})
}

/// Where in the gate's order a request falls: `process` is the facts' read
/// first and the re-read after the paste second.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Step {
    Facts,
    Process,
    Paste,
    Reread,
    Enter,
    Other,
}

type Answer = Option<Result<Value, Fault>>;

/// The block of `fixture`, which reports `working` after the Enter unless
/// `hook` answers or queues otherwise.
fn gate(
    fixture: Value,
    hook: impl Fn(Step, &Request, &mut Events) -> Answer + Send + Sync + 'static,
) -> Script {
    let base = one_block(fixture);
    let reads = AtomicUsize::new(0);
    Script::new(move |request, events| {
        let step = match request.short() {
            "program_status" => Step::Facts,
            "process" if reads.fetch_add(1, Ordering::SeqCst) == 0 => Step::Process,
            "process" => Step::Reread,
            "block.event" if request.payload["event"]["type"] == "paste" => Step::Paste,
            "block.event" => Step::Enter,
            _ => Step::Other,
        };
        if let Some(answer) = hook(step, request, events) {
            return answer;
        }
        match step {
            Step::Enter => {
                events.later(status("working"));
                Ok(json!({}))
            }
            Step::Paste => Ok(json!({})),
            _ => base.answer(request, events),
        }
    })
}

fn quiet(_: Step, _: &Request, _: &mut Events) -> Answer {
    None
}

struct Gate {
    _state: State,
    rex: RexMux<Script>,
}

/// `fixture`'s block bound to `session-1` with the incarnations given, and
/// a script made with the block's local processes in hand.
fn set_up(
    name: &str,
    foreground: &str,
    agent: &str,
    script: impl FnOnce(Value, Arc<Local>) -> Script,
) -> Gate {
    let state = State::new();
    let fixture = fixture(name);
    state.bind(&binding(foreground, agent, "claude-code"));
    let local = Arc::new(Local::from_fixture(&fixture));
    let rex = rex_shared(script(fixture, Arc::clone(&local)), &state, local);
    Gate { _state: state, rex }
}

/// The stand-in agent, bound as its own foreground.
fn bound(script: impl FnOnce(Value, Arc<Local>) -> Script) -> Gate {
    set_up("stale-before", "75361@75361000", "75361@75361000", script)
}

/// The agent behind the `bash` wrapper, which is the foreground.
fn launched(script: impl FnOnce(Value, Arc<Local>) -> Script) -> Gate {
    set_up("wrapper-before", "70962@70962000", "70969@70969000", script)
}

fn claude() -> AgentIdentity {
    AgentIdentity {
        kind: AgentKind::Known(AgentRuntime::ClaudeCode),
        label: Some("claude-code".into()),
        session: SessionClaim::Reported(AgentSessionId::new("session-1")),
    }
}

fn prompt(rex: &RexMux<Script>, text: &str) -> InputOutcome {
    let expect = claude();
    rex.send(
        &PaneId::new("block:b"),
        InputKind::Prompt(PromptRequest {
            expect: &expect,
            text,
        }),
    )
}

fn typed(rex: &RexMux<Script>) -> Vec<String> {
    rex.wire
        .calls()
        .into_iter()
        .filter(|call| call.starts_with("block.event"))
        .collect()
}

#[test]
fn a_prompt_reads_the_facts_on_the_tapped_connection_then_pastes_rechecks_and_presses_enter() {
    let gate = bound(|fixture, _| gate(fixture, quiet));
    assert_eq!(prompt(&gate.rex, "hello\nworld"), InputOutcome::Delivered);
    assert_eq!(
        gate.rex.wire.calls(),
        [
            "server.status",
            "target.resolve block:b",
            "session.attach session:s",
            "program_status block:b",
            "process block:b",
            "block.event paste hello world",
            "process block:b",
            "block.event key enter",
        ]
    );
}

#[test]
fn a_prompt_to_an_agent_behind_a_live_launcher_is_delivered() {
    let gate = launched(|fixture, _| gate(fixture, quiet));
    assert_eq!(prompt(&gate.rex, "hello"), InputOutcome::Delivered);
}

#[test]
fn a_prompts_control_characters_are_pasted_as_spaces() {
    let gate = bound(|fixture, _| gate(fixture, quiet));
    let text = "@bob says\n  \"$(echo hi)\"\r\tdone\u{7f}\u{1b}[A";
    assert_eq!(prompt(&gate.rex, text), InputOutcome::Delivered);
    assert_eq!(
        typed(&gate.rex)[0],
        "block.event paste @bob says   \"$(echo hi)\"  done  [A"
    );
    assert_eq!(one_line("a\u{0}b\u{1f}c"), "a b c");
}

#[test]
fn an_event_the_facts_already_include_is_skipped() {
    let gate = bound(|fixture, _| {
        gate(fixture, |step, _, events| {
            if step == Step::Facts {
                events.earlier(status("working"));
                events.earlier(block_event("child_exited", json!({"exit_code": 0})));
            }
            None
        })
    });
    assert_eq!(prompt(&gate.rex, "hello"), InputOutcome::Delivered);
}

#[test]
fn a_status_change_after_the_facts_declines_before_any_input() {
    for (why, at) in [
        ("between the two facts reads", Step::Facts),
        ("after the second facts read", Step::Process),
    ] {
        let gate = bound(|fixture, _| {
            gate(fixture, move |step, _, events| {
                if step == at {
                    events.later(status("working"));
                }
                None
            })
        });
        assert_eq!(
            prompt(&gate.rex, "hello"),
            InputOutcome::Declined(
                Retry::OnEvent,
                "pane block:b changed status before the prompt".into()
            ),
            "{why}"
        );
        assert!(typed(&gate.rex).is_empty(), "{why}");
    }
}

#[test]
fn a_program_change_after_the_facts_declines_before_any_input() {
    for event in [
        block_event(
            "program_status_removed",
            json!({"ids": [""], "reason": "foreground"}),
        ),
        block_event("child_exited", json!({"exit_code": -1})),
        block_event(
            "process_changed",
            json!({"generation": 3, "foreground": {"pid": 70944}}),
        ),
        block_event(
            "process_changed",
            json!({"generation": 3, "foreground": null}),
        ),
    ] {
        let queued = event.clone();
        let gate = bound(|fixture, _| {
            gate(fixture, move |step, _, events| {
                if step == Step::Process {
                    events.later(queued.clone());
                }
                None
            })
        });
        assert_eq!(
            prompt(&gate.rex, "hello"),
            InputOutcome::Declined(
                Retry::OnEvent,
                "the program in pane block:b changed before the prompt".into()
            ),
            "{event}"
        );
        assert!(typed(&gate.rex).is_empty(), "{event}");
    }
}

#[test]
fn a_process_change_that_keeps_the_foreground_and_other_events_are_skipped() {
    let gate = bound(|fixture, _| {
        gate(fixture, |step, _, events| {
            if step == Step::Process {
                events.later(block_event(
                    "process_changed",
                    json!({"generation": 3, "foreground": {"pid": 75361}}),
                ));
                let mut other = status("working");
                other["payload"]["event"]["payload"]["block_id"] = json!("block:other");
                events.later(other);
                events.later(block_event("title_changed", json!({"title": "x"})));
                events.later(block_event(
                    "program_status_changed",
                    json!({"id": "build", "record": {"state": "working"}}),
                ));
            }
            None
        })
    });
    assert_eq!(prompt(&gate.rex, "hello"), InputOutcome::Delivered);
}

/// Rex queued the change before it started the paste, so the agent was
/// already busy. Read after the paste, it must never confirm.
#[test]
fn a_status_change_queued_before_the_paste_holds_without_an_enter() {
    for state in ["working", "idle"] {
        let gate = bound(|fixture, _| {
            gate(fixture, move |step, _, events| {
                if step == Step::Paste {
                    events.earlier(status(state));
                }
                None
            })
        });
        assert!(
            matches!(prompt(&gate.rex, "hello"), InputOutcome::Ambiguous(why)
                if why.contains("changed before the Enter")),
            "{state}"
        );
        assert_eq!(typed(&gate.rex), ["block.event paste hello"], "{state}");
    }
}

#[test]
fn only_working_after_the_enter_delivers() {
    let gate = bound(|fixture, _| {
        gate(fixture, |step, _, events| {
            (step == Step::Enter).then(|| {
                events.later(status("idle"));
                events.later(status("done"));
                Ok(json!({}))
            })
        })
    });
    let started = std::time::Instant::now();
    assert!(matches!(
        prompt(&gate.rex, "hello"),
        InputOutcome::Ambiguous(why) if why.contains("did not report working")
    ));
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
}

#[test]
fn a_program_change_after_the_enter_is_ambiguous_at_once() {
    let gate = bound(|fixture, _| {
        gate(fixture, |step, _, events| {
            (step == Step::Enter).then(|| {
                events.later(block_event("child_exited", json!({"exit_code": 0})));
                events.later(status("working"));
                Ok(json!({}))
            })
        })
    });
    assert_eq!(
        prompt(&gate.rex, "hello"),
        InputOutcome::Ambiguous("the program in pane block:b changed after the prompt".into())
    );
}

#[test]
fn another_foreground_at_the_reread_holds_without_an_enter() {
    let gate = bound(|fixture, _| {
        let mut zsh = fixture["process"].clone();
        zsh["foreground"]["pid"] = json!(70944);
        gate(fixture, move |step, _, _| {
            (step == Step::Reread).then(|| Ok(zsh.clone()))
        })
    });
    assert!(matches!(
        prompt(&gate.rex, "hello"),
        InputOutcome::Ambiguous(why) if why.contains("foreground of block block:b changed")
    ));
    assert_eq!(typed(&gate.rex), ["block.event paste hello"]);
}

#[test]
fn a_bound_agent_that_did_not_survive_the_paste_holds_without_an_enter() {
    for (why, pid, after) in [
        ("gone behind its launcher", 70969, Some(LocalRead::Gone)),
        ("a zombie behind its launcher", 70969, None),
        (
            "unreadable behind its launcher",
            70969,
            Some(LocalRead::Unreadable),
        ),
        (
            "restarted behind its launcher",
            70969,
            Some(restarted(70969, 70962)),
        ),
        (
            "out of the launcher's group",
            70969,
            Some(restarted_in(70969, 70_969_000, 1)),
        ),
        (
            "an agent that is the foreground, restarted",
            75361,
            Some(restarted(75361, 75361)),
        ),
    ] {
        let script = move |fixture: Value, local: Arc<Local>| {
            gate(fixture, move |step, _, _| {
                if step == Step::Reread {
                    match &after {
                        Some(read) => local.set(pid, read.clone()),
                        // A zombie reads as gone and stays listed in its group.
                        None => local.set(pid, LocalRead::Gone),
                    }
                }
                None
            })
        };
        let gate = if pid == 75361 {
            bound(script)
        } else {
            launched(script)
        };
        assert!(
            matches!(prompt(&gate.rex, "hello"), InputOutcome::Ambiguous(why)
                if why.contains("no longer the one the facts admitted")),
            "{why}"
        );
        assert_eq!(typed(&gate.rex), ["block.event paste hello"], "{why}");
    }
}

fn restarted(pid: u32, group: u32) -> LocalRead {
    restarted_in(pid, 1, group)
}

fn restarted_in(pid: u32, start: u64, group: u32) -> LocalRead {
    LocalRead::Alive(crate::mux::processes::LocalProc {
        incarnation: crate::mux::Incarnation(format!("{pid}@{start}")),
        group,
    })
}

#[test]
fn a_failed_paste_is_ambiguous_and_presses_no_enter() {
    for fault in [
        rex_error("not_found"),
        Fault::Closed("the connection ended".into()),
    ] {
        let failed = fault.clone();
        let gate = bound(|fixture, _| {
            gate(fixture, move |step, _, _| {
                (step == Step::Paste).then(|| Err(failed.clone()))
            })
        });
        assert!(
            matches!(prompt(&gate.rex, "hello"), InputOutcome::Ambiguous(why)
                if why.starts_with("the prompt may have reached pane block:b")),
            "{fault:?}"
        );
        assert_eq!(typed(&gate.rex), ["block.event paste hello"]);
    }
}

#[test]
fn a_connection_too_short_to_confirm_declines_before_it_connects() {
    let gate = bound(|fixture, _| gate(fixture, quiet));
    gate.rex.wire.timeout_ms.store(5999, Ordering::Relaxed);
    assert_eq!(
        prompt(&gate.rex, "hello"),
        InputOutcome::Declined(
            Retry::AfterDelay,
            "a prompt needs 6s to confirm, and this connection allows 5999ms".into()
        )
    );
    assert!(gate.rex.wire.calls().is_empty());
}

/// The Stop wrapper's 250 ms caps each request of the processes that carry
/// it, so it declines their prompts before Namedrop dials. The 5 s each
/// request gets when the variable is unset does not count.
#[test]
fn only_a_set_rex_timeout_counts_in_the_confirm_check() {
    use crate::mux::{
        Budget, Endpoint,
        rex::{Socket, tests::State},
    };
    use std::{os::unix::net::UnixListener, time::Duration};
    let state = State::new();
    let path = state.0.join("rex.sock");
    let listener = UnixListener::bind(&path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let prompt_with = |set_cap| {
        let socket = Socket {
            path: path.clone(),
            budget: Budget::ADAPTER_DEFAULT,
            cancelled: Arc::default(),
            set_cap,
        };
        let endpoint = Endpoint::parse(&format!("rex:{}", path.display()));
        let rex = RexMux::new(socket, endpoint, &state.0, Arc::new(Local::default()));
        let expect = claude();
        rex.send(
            &PaneId::new("block:b"),
            InputKind::Prompt(PromptRequest {
                expect: &expect,
                text: "hello",
            }),
        )
    };
    assert_eq!(
        prompt_with(Some(Duration::from_millis(250))),
        InputOutcome::Declined(
            Retry::AfterDelay,
            "a prompt needs 6s to confirm, and this connection allows 250ms".into()
        )
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock,
        "the declined prompt dialed Rex"
    );
    drop(listener);
    std::fs::remove_file(&path).unwrap();
    assert!(
        matches!(prompt_with(None), InputOutcome::Declined(Retry::AfterDelay, why)
            if why.starts_with("could not reach Rex at")),
        "an unset variable declined the prompt before it dialed"
    );
}

/// A connection that fails before the paste typed nothing, so the prompt
/// waits for a timed retry, not for an event the tap can no longer see.
#[test]
fn a_connection_fault_at_the_check_before_the_paste_retries_after_a_delay() {
    let gate = bound(|fixture, _| {
        gate(fixture, |step, _, events| {
            if step == Step::Process {
                events.broken(Fault::Closed("the connection ended".into()));
            }
            None
        })
    });
    assert_eq!(
        prompt(&gate.rex, "hello"),
        InputOutcome::Declined(
            Retry::AfterDelay,
            "could not watch pane block:b: the connection ended".into()
        )
    );
    assert!(typed(&gate.rex).is_empty());
}

#[test]
fn input_for_a_block_rex_does_not_have_waits_for_an_event() {
    let gate = bound(|fixture, _| {
        gate(fixture, |_, request, _| {
            (request.short() == "target.resolve").then(|| {
                Ok(json!({"results": [{"query": "block:b", "status": "not_found", "reason": ""}]}))
            })
        })
    });
    assert_eq!(
        prompt(&gate.rex, "hello"),
        InputOutcome::Declined(Retry::OnEvent, "Rex has no block:b".into())
    );
    assert!(typed(&gate.rex).is_empty());
}

#[test]
fn no_input_reaches_a_block_on_another_machine() {
    let remote = |fixture: Value, _: Arc<Local>| {
        gate(fixture, |_, request, _| {
            (request.short() == "session.attach").then(|| {
                Ok(json!({"view": {"windows": [{"layers": [{"blocks": [
                    {"block_id": "block:b", "host_id": "another-host"}]}]}]}}))
            })
        })
    };
    let refused = "block block:b runs on another machine, and Namedrop types only into blocks on \
                   this one";
    let gate = bound(remote);
    assert_eq!(
        prompt(&gate.rex, "hello"),
        InputOutcome::Refused(refused.into())
    );
    assert_eq!(
        gate.rex
            .send(&PaneId::new("block:b"), InputKind::Line("echo hi")),
        InputOutcome::Refused(refused.into())
    );
    assert!(typed(&gate.rex).is_empty());
}

#[test]
fn a_terminal_line_is_one_paste_and_an_enter() {
    let gate = bound(|fixture, _| gate(fixture, quiet));
    assert_eq!(
        gate.rex
            .send(&PaneId::new("block:b"), InputKind::Line("echo\tone")),
        InputOutcome::Delivered
    );
    assert_eq!(
        gate.rex.wire.calls(),
        [
            "server.status",
            "target.resolve block:b",
            "session.attach session:s",
            "block.event paste echo one",
            "block.event key enter",
        ]
    );
}

#[test]
fn a_terminal_line_with_a_line_break_is_refused_before_any_byte() {
    for line in ["echo one\necho two", "echo one\recho two"] {
        let gate = bound(|fixture, _| gate(fixture, quiet));
        assert!(matches!(
            gate.rex.send(&PaneId::new("block:b"), InputKind::Line(line)),
            InputOutcome::Refused(why) if why.contains("holds a line break")
        ));
        assert!(gate.rex.wire.calls().is_empty(), "{line:?}");
    }
}

#[test]
fn a_rex_error_on_the_first_input_is_refused_and_any_later_failure_is_ambiguous() {
    let failing = |nth: usize, fault: Fault| {
        let seen = AtomicUsize::new(0);
        move |fixture: Value, _: Arc<Local>| {
            gate(fixture, move |step, _, _| {
                if matches!(step, Step::Paste | Step::Enter)
                    && seen.fetch_add(1, Ordering::SeqCst) == nth
                {
                    return Some(Err(fault.clone()));
                }
                matches!(step, Step::Paste | Step::Enter).then(|| Ok(json!({})))
            })
        }
    };
    let line = |gate: &Gate| {
        gate.rex
            .send(&PaneId::new("block:b"), InputKind::Line("ls"))
    };
    let keys = |gate: &Gate| {
        gate.rex.send(
            &PaneId::new("block:b"),
            InputKind::Keys {
                recipient: Recipient::Agent,
                keys: &[Key::Escape, Key::CtrlU],
            },
        )
    };
    assert!(matches!(
        line(&bound(failing(0, rex_error("bad_request")))),
        InputOutcome::Refused(_)
    ));
    let unavailable = Fault::Rex {
        code: "unavailable".into(),
        message: "Rex says unavailable".into(),
        status: 503,
    };
    for fault in [
        Fault::Timeout(std::time::Duration::from_secs(5)),
        rex_error("timeout"),
        rex_error("internal"),
        unavailable,
    ] {
        assert!(
            matches!(
                line(&bound(failing(0, fault.clone()))),
                InputOutcome::Ambiguous(_)
            ),
            "{fault:?}"
        );
    }
    assert!(matches!(
        line(&bound(failing(1, rex_error("bad_request")))),
        InputOutcome::Ambiguous(_)
    ));
    assert!(matches!(
        keys(&bound(failing(0, rex_error("bad_request")))),
        InputOutcome::Refused(_)
    ));
    assert!(matches!(
        keys(&bound(failing(1, rex_error("bad_request")))),
        InputOutcome::Ambiguous(_)
    ));
}

#[test]
fn keys_are_sent_in_order_to_an_agent_or_a_pane_alike() {
    for recipient in [Recipient::Agent, Recipient::Pane] {
        let gate = bound(|fixture, _| gate(fixture, quiet));
        let keys = [
            Key::Escape,
            Key::Up,
            Key::Down,
            Key::CtrlC,
            Key::CtrlU,
            Key::Enter,
        ];
        assert_eq!(
            gate.rex.send(
                &PaneId::new("block:b"),
                InputKind::Keys {
                    recipient,
                    keys: &keys,
                },
            ),
            InputOutcome::Delivered
        );
        assert_eq!(
            typed(&gate.rex),
            [
                "block.event key escape",
                "block.event key arrow_up",
                "block.event key arrow_down",
                "block.event key key_c",
                "block.event key key_u",
                "block.event key enter",
            ]
        );
    }
}

/// The payloads a scratch block running `stty raw -echo; od -c` turned into
/// `\r`, `033`, `033 [ A`, `033 [ B`, `003`, and `025`.
#[test]
fn each_key_sends_the_payload_measured_on_a_real_block() {
    let press = |key: &str, mods: &str, text: &str, code: u32| {
        json!({"key": key, "mods": mods, "consumed_mods": "", "text": text,
               "unshifted_codepoint": code, "composing": false, "action": "press"})
    };
    for (key, payload) in [
        (Key::Enter, press("enter", "", "\r", 13)),
        (Key::Escape, press("escape", "", "", 27)),
        (Key::Up, press("arrow_up", "", "", 0)),
        (Key::Down, press("arrow_down", "", "", 0)),
        (Key::CtrlC, press("key_c", "ctrl", "", 99)),
        (Key::CtrlU, press("key_u", "ctrl", "", 117)),
    ] {
        assert_eq!(key_payload(key), payload, "{key:?}");
    }
}

#[test]
fn identify_labels_the_block_and_rename_tab_labels_the_window() {
    let gate = bound(|fixture, _| {
        gate(fixture, |_, request, _| match request.short() {
            "session.set_block_label" | "session.set_window_label" => Some(Ok(json!({}))),
            "target.resolve" if request.payload["kinds"] == json!(["window"]) => Some(Ok(json!(
                {"results": [{"query": "window:w", "status": "resolved",
                 "target": {"kind": "window", "session_id": "session:s", "label": "old"}}]}
            ))),
            _ => None,
        })
    });
    let texts = StateTexts {
        ready: "ready".into(),
        working: "working".into(),
        attention: "attention".into(),
        unknown: "unknown".into(),
    };
    let identify = Effect::Identify {
        pane: PaneId::new("block:b"),
        alias: "bob".into(),
        texts,
    };
    assert_eq!(gate.rex.apply(&identify), Ok(Applied::Done));
    let rename = Effect::RenameTab {
        tab: TabId::new("window:w"),
        label: "@bob".into(),
    };
    assert_eq!(gate.rex.apply(&rename), Ok(Applied::Done));
    assert_eq!(
        gate.rex.wire.calls(),
        [
            "server.status",
            "target.resolve block:b",
            "session.attach session:s",
            "session.set_block_label block:b @bob",
            "target.resolve window:w",
            "session.attach session:s",
            "session.set_window_label window:w @bob",
        ]
    );
}

#[test]
fn effects_rex_cannot_show_are_unsupported_without_a_connection() {
    let gate = bound(|fixture, _| gate(fixture, quiet));
    for effect in [
        Effect::Badges {
            pane: PaneId::new("block:b"),
            badges: vec![Badge::Awaiting(1)],
        },
        Effect::Notify {
            title: "t".into(),
            body: "b".into(),
            sound: false,
        },
        Effect::ReloadKeybindings,
    ] {
        assert_eq!(
            gate.rex.apply(&effect),
            Ok(Applied::Unsupported),
            "{effect:?}"
        );
    }
    assert!(gate.rex.wire.calls().is_empty());
}

/// The channel's drain normally finds such an event first. The tap holds the
/// same rule for one that reaches the socket only after the drain.
#[test]
fn the_tap_reports_a_status_change_queued_before_the_enter_as_a_program_change() {
    use super::{RexTap, Seen};
    use crate::mux::{
        AgentState,
        gate::{StatusTap, TapChange},
        rex::{
            Call,
            control::{Event, Narrower, Wire},
        },
    };
    use std::{cell::RefCell, rc::Rc, time::Duration};
    let script = Script::new(|_, _| Ok(json!({})));
    let (control, _) = script.open(Narrower::none()).unwrap();
    let seen = Seen {
        boundary: 1,
        entered: Some(3),
        foreground: Some(75361),
        ..Seen::default()
    };
    let mut tap = RexTap {
        call: Rc::new(RefCell::new(Call {
            control,
            host: None,
        })),
        block: "block:b".into(),
        seen: Rc::new(RefCell::new(seen)),
    };
    for (seq, state) in [(1, "idle"), (2, "working"), (4, "working")] {
        tap.seen.borrow_mut().pending.push_back(Event {
            seq,
            payload: status(state),
        });
    }
    assert_eq!(
        tap.next_change(Duration::ZERO),
        Ok(Some(TapChange::Program))
    );
    assert_eq!(
        tap.next_change(Duration::ZERO),
        Ok(Some(TapChange::Status(AgentState::Working)))
    );
    assert_eq!(tap.next_change(Duration::ZERO), Ok(None));
}

/// Rex queued the change after it started the paste, so the Enter did not
/// cause it. Read before the Enter, it stops the Enter. Queued before the
/// Enter started and read after it, it never confirms.
#[test]
fn a_change_between_the_paste_and_the_enter_never_confirms() {
    for state in ["working", "blocked"] {
        for during in [Step::Paste, Step::Reread] {
            let read_first = bound(|fixture, _| {
                gate(fixture, move |step, _, events| {
                    if step == during {
                        events.later(status(state));
                    }
                    None
                })
            });
            assert!(
                matches!(prompt(&read_first.rex, "hello"), InputOutcome::Ambiguous(why)
                    if why.contains("changed before the Enter")),
                "{state} {during:?}"
            );
            assert_eq!(
                typed(&read_first.rex),
                ["block.event paste hello"],
                "{state} {during:?}"
            );
        }

        let read_after = bound(|fixture, _| {
            gate(fixture, move |step, _, events| {
                (step == Step::Enter).then(|| {
                    events.earlier(status(state));
                    Ok(json!({}))
                })
            })
        });
        assert!(
            matches!(prompt(&read_after.rex, "hello"), InputOutcome::Ambiguous(why)
                if why.contains("changed after the prompt")),
            "{state}"
        );
    }
}
