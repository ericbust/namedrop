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
    /// Reports only changes that happen after it opens.
    fn tap(&self, pane: &PaneId) -> Result<Self::Tap, ChannelError>;
    /// Pastes `text` and presses Enter on one connection.
    fn paste_submit(&self, pane: &PaneId, text: &str) -> Result<(), ChannelError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TapChange {
    Status(AgentState),
    /// The program the facts named may no longer run in the pane, as when a
    /// status record outlives the agent that wrote it.
    Program,
}

pub trait StatusTap {
    /// The next change, or `None` when none arrives within `within`. An
    /// error means the tap can no longer watch the pane.
    fn next_change(&mut self, within: Duration) -> Result<Option<TapChange>, ChannelError>;
}

/// Admits only the agent the caller verified, in a status that accepts a
/// prompt. An agent that reports `working` is declined even between turns,
/// because a change to `Working` after the paste could not confirm a prompt
/// typed while the agent already reported `working`.
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
    match tap.next_change(Duration::ZERO) {
        Err(ChannelError(error)) => {
            return InputOutcome::Declined(
                Retry::AfterDelay,
                format!("could not watch pane {pane_name}: {error}"),
            );
        }
        Ok(Some(TapChange::Status(_))) => {
            return InputOutcome::Declined(
                Retry::OnEvent,
                format!("pane {pane_name} changed status before the prompt"),
            );
        }
        Ok(Some(TapChange::Program)) => {
            return InputOutcome::Declined(
                Retry::OnEvent,
                format!("the program in pane {pane_name} changed before the prompt"),
            );
        }
        Ok(None) => {}
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
            Ok(Some(TapChange::Status(AgentState::Working))) => return InputOutcome::Delivered,
            Ok(Some(TapChange::Program)) => {
                return InputOutcome::Ambiguous(format!(
                    "the program in pane {pane_name} changed after the prompt"
                ));
            }
            Ok(Some(TapChange::Status(_))) => {}
            Ok(None) => break,
            Err(ChannelError(error)) => {
                return InputOutcome::Ambiguous(format!(
                    "the prompt may have reached pane {pane_name}, and its watch failed: {error}"
                ));
            }
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
        mux::{AgentKind, Incarnation, SessionClaim},
    };
    use std::{cell::RefCell, rc::Rc};

    #[derive(Clone, Copy)]
    enum AfterPaste {
        StartsWorking,
        /// The snapshot says working, but no status change follows the paste.
        StaysStaleWorking,
        ProgramChangesThenWorking,
        Fails,
        WatchLost,
    }

    struct Pane {
        agent: Option<AgentIdentity>,
        state: Option<AgentState>,
        changes: Vec<TapChange>,
        pasted: Vec<String>,
        watch_lost: bool,
    }

    struct Scripted {
        pane: Rc<RefCell<Pane>>,
        after_paste: AfterPaste,
        facts_fail: bool,
        tap_fails: bool,
        changes_after_facts: Vec<TapChange>,
        watch_lost_after_facts: bool,
    }

    struct Tap {
        pane: Rc<RefCell<Pane>>,
        next: usize,
    }

    impl StatusTap for Tap {
        fn next_change(&mut self, _within: Duration) -> Result<Option<TapChange>, ChannelError> {
            if self.pane.borrow().watch_lost {
                return Err(ChannelError("the connection ended".into()));
            }
            let change = self.pane.borrow().changes.get(self.next).cloned();
            if change.is_some() {
                self.next += 1;
            }
            Ok(change)
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
            pane.watch_lost = self.watch_lost_after_facts;
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
                    pane.changes.push(TapChange::Status(AgentState::Working));
                    Ok(())
                }
                AfterPaste::StaysStaleWorking => {
                    pane.state = Some(AgentState::Working);
                    Ok(())
                }
                AfterPaste::ProgramChangesThenWorking => {
                    pane.changes
                        .extend([TapChange::Program, TapChange::Status(AgentState::Working)]);
                    Ok(())
                }
                AfterPaste::Fails => Err(ChannelError("connection reset".into())),
                AfterPaste::WatchLost => {
                    pane.watch_lost = true;
                    Ok(())
                }
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
                watch_lost: false,
            })),
            after_paste: after,
            facts_fail: false,
            tap_fails: false,
            changes_after_facts: Vec::new(),
            watch_lost_after_facts: false,
        }
    }

    fn prompt(channel: &Scripted, expect: &AgentIdentity) -> InputOutcome {
        let request = PromptRequest {
            expect,
            text: "hello",
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
                prompt(&channel, &agent),
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
                    prompt(&channel, &agent),
                    InputOutcome::Declined(Retry::OnEvent, _)
                ),
                "{status:?}"
            );
            assert!(pasted(&channel).is_empty(), "{status:?}");
        }
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
                    prompt(&channel, &expected),
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
            prompt(&channel, &agent),
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
        channel.pane.borrow_mut().changes.extend([
            TapChange::Status(AgentState::Working),
            TapChange::Status(AgentState::Idle),
        ]);
        assert!(matches!(
            prompt(&channel, &agent),
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
        let mut watch_lost = channel(Some(agent.clone()), Some("idle"), AfterPaste::StartsWorking);
        watch_lost.watch_lost_after_facts = true;
        for channel in [facts_fail, tap_fails, watch_lost] {
            assert!(matches!(
                prompt(&channel, &agent),
                InputOutcome::Declined(Retry::AfterDelay, _)
            ));
            assert!(pasted(&channel).is_empty());
        }
    }

    #[test]
    fn a_status_change_after_the_facts_are_read_declines_before_any_input() {
        let agent = claude(reported("session-1"));
        let mut channel = channel(Some(agent.clone()), Some("idle"), AfterPaste::StartsWorking);
        channel.changes_after_facts = vec![TapChange::Status(AgentState::Working)];
        assert!(matches!(
            prompt(&channel, &agent),
            InputOutcome::Declined(Retry::OnEvent, _)
        ));
        assert!(pasted(&channel).is_empty());
    }

    #[test]
    fn a_program_change_after_the_facts_are_read_declines_before_any_input() {
        let agent = claude(reported("session-1"));
        let mut channel = channel(Some(agent.clone()), Some("idle"), AfterPaste::StartsWorking);
        channel.changes_after_facts = vec![TapChange::Program];
        assert_eq!(
            prompt(&channel, &agent),
            InputOutcome::Declined(
                Retry::OnEvent,
                "the program in pane pane-1 changed before the prompt".into()
            )
        );
        assert!(pasted(&channel).is_empty());
    }

    #[test]
    fn a_watch_lost_after_the_paste_is_ambiguous_at_once() {
        let agent = claude(reported("session-1"));
        let channel = channel(Some(agent.clone()), Some("idle"), AfterPaste::WatchLost);
        assert_eq!(
            prompt(&channel, &agent),
            InputOutcome::Ambiguous(
                "the prompt may have reached pane pane-1, and its watch failed: the connection ended"
                    .into()
            )
        );
        assert_eq!(pasted(&channel), ["hello"]);
    }

    #[test]
    fn a_program_change_after_the_paste_is_ambiguous_at_once() {
        let agent = claude(reported("session-1"));
        let channel = channel(
            Some(agent.clone()),
            Some("idle"),
            AfterPaste::ProgramChangesThenWorking,
        );
        assert_eq!(
            prompt(&channel, &agent),
            InputOutcome::Ambiguous("the program in pane pane-1 changed after the prompt".into())
        );
        assert_eq!(pasted(&channel), ["hello"]);
    }
}
