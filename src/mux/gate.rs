use super::{AgentIdentity, AgentState, InputOutcome, PaneId, PromptRequest, Retry};
use std::time::{Duration, Instant};

pub const CONFIRM_WINDOW: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChannelError(pub String);

/// What a multiplexer without a native prompt gate provides. `emulate_prompt`
/// owns the order of these calls and the ambiguity rule. It opens the tap
/// before it reads the facts, so a status change between the read and the
/// paste declines the prompt instead of typing into a busy agent.
pub trait RawChannel {
    type Tap: StatusTap;

    /// The pane's agent and status now.
    fn facts(
        &self,
        pane: &PaneId,
    ) -> Result<(Option<AgentIdentity>, Option<AgentState>), ChannelError>;
    /// Reports only status changes that happen after it opens.
    fn tap(&self, pane: &PaneId) -> Result<Self::Tap, ChannelError>;
    /// Pastes `text` and presses Enter on one connection.
    fn paste_submit(&self, pane: &PaneId, text: &str) -> Result<(), ChannelError>;
}

pub trait StatusTap {
    /// The next status change, or `None` when none arrives within `within`.
    fn next_change(&mut self, within: Duration) -> Option<AgentState>;
}

/// Admits only the agent the caller verified, in a status that accepts a
/// prompt. The status decides even for `Admit::BetweenTurns`, because a
/// change to `Working` after the paste could not confirm a prompt typed while
/// the agent already reported `working`.
fn admits(
    agent: Option<&AgentIdentity>,
    state: Option<&AgentState>,
    expect: &AgentIdentity,
) -> bool {
    agent.is_some_and(|agent| agent.kind == expect.kind && agent.session == expect.session)
        && state.is_some_and(AgentState::accepts_prompt)
}

pub fn emulate_prompt(
    channel: &impl RawChannel,
    pane: &PaneId,
    request: &PromptRequest<'_>,
) -> InputOutcome {
    let pane_name = pane.as_str();
    let mut tap = match channel.tap(pane) {
        Ok(tap) => tap,
        Err(ChannelError(error)) => {
            return InputOutcome::Declined(
                Retry::AfterDelay,
                format!("could not watch pane {pane_name}: {error}"),
            );
        }
    };
    let (agent, state) = match channel.facts(pane) {
        Ok(facts) => facts,
        Err(ChannelError(error)) => {
            return InputOutcome::Declined(
                Retry::AfterDelay,
                format!("could not read pane {pane_name}: {error}"),
            );
        }
    };
    if !admits(agent.as_ref(), state.as_ref(), request.expect) {
        return InputOutcome::Declined(
            Retry::OnEvent,
            format!("pane {pane_name} cannot take a prompt now"),
        );
    }
    if tap.next_change(Duration::ZERO).is_some() {
        return InputOutcome::Declined(
            Retry::OnEvent,
            format!("pane {pane_name} changed status before the prompt"),
        );
    }
    if let Err(ChannelError(error)) = channel.paste_submit(pane, request.text) {
        return InputOutcome::Ambiguous(format!(
            "the prompt may have reached pane {}: {error}",
            pane.as_str()
        ));
    }
    let deadline = Instant::now() + CONFIRM_WINDOW;
    while let Some(within) = deadline.checked_duration_since(Instant::now()) {
        match tap.next_change(within) {
            Some(AgentState::Working) => return InputOutcome::Delivered,
            Some(_) => {}
            None => break,
        }
    }
    InputOutcome::Ambiguous(format!(
        "pane {} did not report working within {}s of the prompt",
        pane.as_str(),
        CONFIRM_WINDOW.as_secs()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AgentRuntime, AgentSessionId,
        mux::{Admit, AgentKind, Incarnation, SessionClaim},
    };
    use std::{cell::RefCell, rc::Rc};

    #[derive(Clone, Copy)]
    enum AfterPaste {
        StartsWorking,
        /// The snapshot says working, but no status change follows the paste.
        StaysStaleWorking,
        Fails,
    }

    struct Pane {
        agent: Option<AgentIdentity>,
        state: Option<AgentState>,
        changes: Vec<AgentState>,
        pasted: Vec<String>,
    }

    struct Scripted {
        pane: Rc<RefCell<Pane>>,
        after_paste: AfterPaste,
        facts_fail: bool,
        tap_fails: bool,
        changes_after_facts: Vec<AgentState>,
    }

    struct Tap {
        pane: Rc<RefCell<Pane>>,
        next: usize,
    }

    impl StatusTap for Tap {
        fn next_change(&mut self, _within: Duration) -> Option<AgentState> {
            let change = self.pane.borrow().changes.get(self.next).cloned();
            if change.is_some() {
                self.next += 1;
            }
            change
        }
    }

    impl RawChannel for Scripted {
        type Tap = Tap;

        fn facts(
            &self,
            _pane: &PaneId,
        ) -> Result<(Option<AgentIdentity>, Option<AgentState>), ChannelError> {
            if self.facts_fail {
                return Err(ChannelError("connection refused".into()));
            }
            let mut pane = self.pane.borrow_mut();
            let facts = (pane.agent.clone(), pane.state.clone());
            pane.changes
                .extend(self.changes_after_facts.iter().cloned());
            Ok(facts)
        }

        fn tap(&self, _pane: &PaneId) -> Result<Tap, ChannelError> {
            if self.tap_fails {
                return Err(ChannelError("subscribe failed".into()));
            }
            let next = self.pane.borrow().changes.len();
            Ok(Tap {
                pane: Rc::clone(&self.pane),
                next,
            })
        }

        fn paste_submit(&self, _pane: &PaneId, text: &str) -> Result<(), ChannelError> {
            let mut pane = self.pane.borrow_mut();
            pane.pasted.push(text.to_owned());
            match self.after_paste {
                AfterPaste::StartsWorking => {
                    pane.state = Some(AgentState::Working);
                    pane.changes.push(AgentState::Working);
                    Ok(())
                }
                AfterPaste::StaysStaleWorking => {
                    pane.state = Some(AgentState::Working);
                    Ok(())
                }
                AfterPaste::Fails => Err(ChannelError("connection reset".into())),
            }
        }
    }

    fn claude(session: SessionClaim) -> AgentIdentity {
        AgentIdentity {
            kind: AgentKind::Known(AgentRuntime::ClaudeCode),
            label: Some("claude".into()),
            session,
        }
    }

    fn reported(session: &str) -> SessionClaim {
        SessionClaim::Reported(AgentSessionId::new(session))
    }

    fn channel(agent: Option<AgentIdentity>, status: Option<&str>, after: AfterPaste) -> Scripted {
        Scripted {
            pane: Rc::new(RefCell::new(Pane {
                agent,
                state: status.map(AgentState::parse),
                changes: Vec::new(),
                pasted: Vec::new(),
            })),
            after_paste: after,
            facts_fail: false,
            tap_fails: false,
            changes_after_facts: Vec::new(),
        }
    }

    fn prompt(channel: &Scripted, expect: &AgentIdentity, admit: Admit) -> InputOutcome {
        let request = PromptRequest {
            expect,
            text: "hello",
            admit,
        };
        emulate_prompt(channel, &PaneId::new("pane-1"), &request)
    }

    fn pasted(channel: &Scripted) -> Vec<String> {
        channel.pane.borrow().pasted.clone()
    }

    #[test]
    fn a_settled_agent_that_starts_working_is_delivered() {
        let agent = claude(reported("session-1"));
        for status in ["idle", "done"] {
            let channel = channel(Some(agent.clone()), Some(status), AfterPaste::StartsWorking);
            assert_eq!(
                prompt(&channel, &agent, Admit::Settled),
                InputOutcome::Delivered,
                "{status}"
            );
            assert_eq!(pasted(&channel), ["hello"]);
        }
    }

    #[test]
    fn an_agent_that_cannot_take_a_prompt_is_declined_before_any_input() {
        let agent = claude(reported("session-1"));
        for status in [Some("working"), Some("blocked"), Some("busy"), None] {
            let channel = channel(Some(agent.clone()), status, AfterPaste::StartsWorking);
            assert!(
                matches!(
                    prompt(&channel, &agent, Admit::Settled),
                    InputOutcome::Declined(Retry::OnEvent, _)
                ),
                "{status:?}"
            );
            assert!(pasted(&channel).is_empty(), "{status:?}");
        }
    }

    #[test]
    fn a_between_turns_admission_is_declined_before_any_input() {
        let agent = claude(reported("session-1"));
        let channel = channel(
            Some(agent.clone()),
            Some("working"),
            AfterPaste::StartsWorking,
        );
        assert!(matches!(
            prompt(&channel, &agent, Admit::BetweenTurns),
            InputOutcome::Declined(Retry::OnEvent, _)
        ));
        assert!(pasted(&channel).is_empty());
    }

    #[test]
    fn an_identity_mismatch_is_declined_before_any_input() {
        let expected = claude(SessionClaim::Unreported {
            incarnation: Incarnation("4242".into()),
        });
        let codex = AgentIdentity {
            kind: AgentKind::Known(AgentRuntime::Codex),
            ..expected.clone()
        };
        for found in [
            None,
            Some(codex),
            Some(claude(SessionClaim::Unreported {
                incarnation: Incarnation("4243".into()),
            })),
            Some(claude(SessionClaim::None)),
            Some(claude(reported("session-1"))),
        ] {
            let channel = channel(found.clone(), Some("idle"), AfterPaste::StartsWorking);
            assert!(
                matches!(
                    prompt(&channel, &expected, Admit::Settled),
                    InputOutcome::Declined(Retry::OnEvent, _)
                ),
                "{found:?}"
            );
            assert!(pasted(&channel).is_empty(), "{found:?}");
        }
    }

    #[test]
    fn a_failure_after_the_paste_is_ambiguous() {
        let agent = claude(reported("session-1"));
        let channel = channel(Some(agent.clone()), Some("idle"), AfterPaste::Fails);
        assert!(matches!(
            prompt(&channel, &agent, Admit::Settled),
            InputOutcome::Ambiguous(_)
        ));
        assert_eq!(pasted(&channel), ["hello"]);
    }

    #[test]
    fn a_stale_working_status_cannot_confirm_delivery() {
        let agent = claude(reported("session-1"));
        let channel = channel(
            Some(agent.clone()),
            Some("idle"),
            AfterPaste::StaysStaleWorking,
        );
        channel
            .pane
            .borrow_mut()
            .changes
            .extend([AgentState::Working, AgentState::Idle]);
        assert!(matches!(
            prompt(&channel, &agent, Admit::Settled),
            InputOutcome::Ambiguous(_)
        ));
        assert_eq!(pasted(&channel), ["hello"]);
    }

    #[test]
    fn a_failure_before_the_paste_is_declined_for_a_timed_retry() {
        let agent = claude(reported("session-1"));
        let mut facts_fail = channel(Some(agent.clone()), Some("idle"), AfterPaste::StartsWorking);
        facts_fail.facts_fail = true;
        let mut tap_fails = channel(Some(agent.clone()), Some("idle"), AfterPaste::StartsWorking);
        tap_fails.tap_fails = true;
        for channel in [facts_fail, tap_fails] {
            assert!(matches!(
                prompt(&channel, &agent, Admit::Settled),
                InputOutcome::Declined(Retry::AfterDelay, _)
            ));
            assert!(pasted(&channel).is_empty());
        }
    }

    #[test]
    fn a_status_change_after_the_facts_are_read_declines_before_any_input() {
        let agent = claude(reported("session-1"));
        let mut channel = channel(Some(agent.clone()), Some("idle"), AfterPaste::StartsWorking);
        channel.changes_after_facts = vec![AgentState::Working];
        assert!(matches!(
            prompt(&channel, &agent, Admit::Settled),
            InputOutcome::Declined(Retry::OnEvent, _)
        ));
        assert!(pasted(&channel).is_empty());
    }
}
