//! A `FakeMux` world that is as strict as the Herdr fake a test replaced. That
//! fake answered a few commands and failed every other one. Here a survey the
//! space does not answer and a read the test did not give the world fail when
//! they are made, and a screen answers only the reads of its pane's
//! `ScreenShape`. When the space drops, it fails the test for every input the
//! code under test tried to type and every effect it tried to apply that the
//! old fake would have refused, even if the fake declined it or it failed. It
//! also fails the test for a `script` no input used and a `fail` no call hit.
//!
//! The old fakes recorded every `agent prompt` they were sent. `on_input`
//! sees the same: every attempt, with its outcome, including the prompts the
//! fake's own gate declined. `sent()` holds only what was typed, so a test
//! that asks what the code tried to type reads `attempts()` or an observer,
//! never `sent()`.
//!
//! Hold the space in a fixture field, not behind the fixture's own `Drop`, so
//! that the fixture's cleanup runs before the check can fail. A handle to the
//! world that outlives the space fails the check, since whatever it does next
//! goes unchecked, so the space must drop last:
//!
//! - Fields drop in declaration order, after the fixture's own `Drop`. A
//!   fixture `{ space, connection }` fails the check, because the connection
//!   is still alive when the space drops. Declare the space after every field
//!   that holds a connection, or empty those fields in the fixture's `Drop`.
//! - A thread that holds a connection, such as a dashboard server's, must be
//!   stopped and joined before the space drops. A detached thread fails the
//!   check while it lives, and makes it racy as it exits.
//! - A clone captured by an observer, a read hook, or a `Takes` predicate is
//!   not a handle that outlives the space. The space drops them before it
//!   counts handles.

use super::{Attempt, FakeMux, Read, ScopeKind, SentInput};
use crate::mux::{Depth, Effect};
use std::{mem, ops::Deref, sync::Arc};

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

/// A Herdr listing command, as an old fake matched it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Listing {
    /// `workspace list`
    Spaces,
    /// `workspace get <space>`
    Space,
    /// `worktree list --cwd <path>`
    CheckoutAt,
    /// `worktree list --workspace <space>`
    CheckoutOf,
    /// `pane list`
    AllPanes,
    /// `pane list --workspace <space>`
    PanesOf,
}

/// The listings the Herdr adapter runs for a survey of `kind` at `depth`, so
/// a fixture that counted one of the old fake's commands counts the same
/// surveys. A checkout survey also runs `worktree list --workspace` for each
/// listed space whose listing names no checkout, which is left out here
/// because the count depends on the world.
pub fn listings(kind: ScopeKind, depth: Depth) -> &'static [Listing] {
    match (kind, depth) {
        (ScopeKind::Space, Depth::Spaces) => &[Listing::Space],
        (ScopeKind::Space, Depth::Panes) => &[Listing::CheckoutOf, Listing::PanesOf],
        (ScopeKind::PanesOf, _) => &[Listing::PanesOf],
        (ScopeKind::CheckoutOf, Depth::Spaces) => &[Listing::CheckoutAt, Listing::Spaces],
        (ScopeKind::CheckoutOf, Depth::Panes) => {
            &[Listing::CheckoutAt, Listing::Spaces, Listing::PanesOf]
        }
        (ScopeKind::All, Depth::Spaces) => &[Listing::Spaces],
        (ScopeKind::All, Depth::Panes) => &[Listing::AllPanes],
    }
}

/// Whether `read` is a survey that runs `listing`.
pub fn runs(read: &Read, listing: Listing) -> bool {
    matches!(read, Read::Survey { scope, depth } if listings(scope.kind(), *depth).contains(&listing))
}

type Allow<T> = Box<dyn Fn(&T) -> bool + Send + Sync>;

/// The input and effects the old fake answered. Each `input` and `effect`
/// call allows more. An attempt passes when any allowance accepts it. The
/// allowances run with the world unlocked, so one may read the fake.
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
            .input(|attempt| matches!(attempt.input, SentInput::Prompt(_)))
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
    ///
    /// # Panics
    ///
    /// When `mux` already answers only some surveys, as it does when it is a
    /// clone of another space's world.
    pub fn new(mux: FakeMux, surveys: &[(ScopeKind, Depth)], takes: Takes) -> Self {
        Self {
            mux: mux.strict_reads().strict_surveys(surveys),
            takes,
        }
    }

    /// Judges the world as the space drops. It copies the records and
    /// unlocks the world before any predicate runs, then drops the
    /// predicates and hooks before it counts the handles left.
    fn violations(&mut self) -> Vec<String> {
        let (attempts, effects, scripts, failures) = {
            let world = self.mux.lock();
            (
                world.attempts.clone(),
                world.attempted_effects.clone(),
                world.scripted.clone(),
                world.failures.clone(),
            )
        };
        let takes = mem::replace(&mut self.takes, Takes::nothing());
        let input = attempts
            .iter()
            .filter(|attempt| !takes.input.iter().any(|allow| allow(attempt)))
            .map(|attempt| format!("input the space does not take: {attempt:?}"));
        let effects = effects
            .iter()
            .filter(|effect| !takes.effect.iter().any(|allow| allow(effect)))
            .map(|effect| format!("an effect the space does not take: {effect:?}"));
        let scripts = scripts
            .iter()
            .filter(|(_, outcomes)| !outcomes.is_empty())
            .map(|(pane, outcomes)| {
                format!(
                    "a script for {} that no input used: {outcomes:?}",
                    pane.as_str()
                )
            });
        let failures = failures
            .iter()
            .map(|(op, error)| format!("an injected {op:?} failure that no call hit: {error:?}"));
        let mut violations: Vec<_> = input
            .chain(effects)
            .chain(scripts)
            .chain(failures)
            .collect();
        drop(takes);
        self.mux.release_hooks();
        let others = Arc::strong_count(&self.mux.shared) - 1;
        if others > 0 {
            violations.push(format!(
                "{others} other handle(s) to the world outlived the space, so what they do next \
                 goes unchecked: a connection, a clone, or a thread still holds one"
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
