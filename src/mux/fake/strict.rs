//! A `FakeMux` world that is as strict as the Herdr fake a test replaced. That
//! fake answered a few commands and failed every other one. Here a survey the
//! space does not answer and a read the test did not give the world fail when
//! they are made. When the space drops, it fails the test for every input the
//! code under test tried to type and every effect it tried to apply that the
//! old fake would have refused, even if the fake declined it or it failed.
//!
//! Hold the space in a fixture field, not behind the fixture's own `Drop`, so
//! that the fixture's cleanup runs before the check can fail. Join every
//! thread and drop every connection the fixture made before the space drops:
//! a handle to the world that outlives the space fails the check, since
//! whatever it does next goes unchecked.

use super::{Attempt, FakeMux, ScopeKind};
use crate::mux::{Depth, Effect};
use std::{ops::Deref, sync::Arc};

/// `workspace list`, `worktree list --cwd`, and `pane list --workspace`,
/// which the messaging fake answered.
pub const CHECKOUT_LISTINGS: &[(ScopeKind, Depth)] = &[
    (ScopeKind::All, Depth::Spaces),
    (ScopeKind::CheckoutOf, Depth::Spaces),
    (ScopeKind::CheckoutOf, Depth::Panes),
    (ScopeKind::PanesOf, Depth::Spaces),
    (ScopeKind::PanesOf, Depth::Panes),
];

/// Every `workspace list`, `worktree list`, and `pane list`: every survey but
/// `workspace get`, which is `Scope::Space` at `Depth::Spaces`.
pub const EVERY_LISTING: &[(ScopeKind, Depth)] = &[
    (ScopeKind::All, Depth::Spaces),
    (ScopeKind::All, Depth::Panes),
    (ScopeKind::Space, Depth::Panes),
    (ScopeKind::CheckoutOf, Depth::Spaces),
    (ScopeKind::CheckoutOf, Depth::Panes),
    (ScopeKind::PanesOf, Depth::Spaces),
    (ScopeKind::PanesOf, Depth::Panes),
];

type Allow<T> = Box<dyn Fn(&T) -> bool + Send + Sync>;

/// The input and effects the old fake answered. Each `input` and `effect`
/// call allows more. An attempt passes when any allowance accepts it.
pub struct Takes {
    input: Vec<Allow<Attempt>>,
    effect: Vec<Allow<Effect>>,
}

impl Takes {
    /// Neither `agent prompt` nor `pane report-metadata`, nor anything else.
    pub fn nothing() -> Self {
        Self {
            input: Vec::new(),
            effect: Vec::new(),
        }
    }

    /// `agent prompt` to any pane, and `pane report-metadata`, which carried
    /// a pane's identity and badges.
    pub fn prompts_and_metadata() -> Self {
        Self::nothing()
            .input(|attempt| matches!(attempt.input, super::SentInput::Prompt(_)))
            .effect(|effect| matches!(effect, Effect::Identify { .. } | Effect::Badges { .. }))
    }

    pub fn input(mut self, allow: impl Fn(&Attempt) -> bool + Send + Sync + 'static) -> Self {
        self.input.push(Box::new(allow));
        self
    }

    pub fn effect(mut self, allow: impl Fn(&Effect) -> bool + Send + Sync + 'static) -> Self {
        self.effect.push(Box::new(allow));
        self
    }
}

pub struct StrictSpace {
    mux: FakeMux,
    takes: Takes,
}

impl StrictSpace {
    /// Strict reads, only the `surveys` the old fake answered, and on drop
    /// only the input and effects it `takes`.
    pub fn new(mux: FakeMux, surveys: &[(ScopeKind, Depth)], takes: Takes) -> Self {
        Self {
            mux: mux.strict_reads().strict_surveys(surveys),
            takes,
        }
    }

    fn violations(&self) -> Vec<String> {
        let world = self.mux.lock();
        let input = world
            .attempts
            .iter()
            .filter(|attempt| !self.takes.input.iter().any(|allow| allow(attempt)))
            .map(|attempt| format!("input the space does not take: {attempt:?}"));
        let effects = world
            .attempted_effects
            .iter()
            .filter(|effect| !self.takes.effect.iter().any(|allow| allow(effect)))
            .map(|effect| format!("an effect the space does not take: {effect:?}"));
        let mut violations: Vec<_> = input.chain(effects).collect();
        let others = Arc::strong_count(&self.mux.shared) - 1;
        if others > 0 {
            violations.push(format!(
                "{others} other handle(s) to the world outlived the space, so what they do next \
                 goes unchecked"
            ));
        }
        violations
    }
}

impl Deref for StrictSpace {
    type Target = FakeMux;

    fn deref(&self) -> &FakeMux {
        &self.mux
    }
}

impl Drop for StrictSpace {
    /// While the test is already failing, the violations go to stderr beside
    /// its own message, since a second panic would abort the test binary.
    fn drop(&mut self) {
        let violations = self.violations();
        if violations.is_empty() {
            return;
        }
        let report = format!(
            "the strict space saw {} violation(s):\n{}",
            violations.len(),
            violations.join("\n")
        );
        if std::thread::panicking() {
            eprintln!("{report}");
        } else {
            panic!("{report}");
        }
    }
}

#[cfg(test)]
mod tests;
