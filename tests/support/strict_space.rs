#![allow(dead_code)]
//! A `FakeMux` world that is as strict as the Herdr fake a test replaced. That
//! fake answered a few commands and failed every other one. Here a read the
//! test did not give the world fails, and when the space drops it fails the
//! test if the code under test typed input or applied an effect that the old
//! fake would have refused.
//!
//! Hold the space in a fixture field, not behind the fixture's own `Drop`, so
//! that the fixture's cleanup runs before the check can fail.

use namedrop::mux::{
    Effect,
    fake::{FakeMux, SentInput},
};
use std::ops::Deref;

/// What the old fake answered besides space, checkout, and pane listings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Takes {
    /// Neither `agent prompt` nor `pane report-metadata`.
    Nothing,
    /// `agent prompt`, and `pane report-metadata`, which carried a pane's
    /// identity and badges.
    PromptsAndMetadata,
}

impl Takes {
    fn input(self, input: &SentInput) -> bool {
        self == Self::PromptsAndMetadata && matches!(input, SentInput::Prompt(_))
    }

    fn effect(self, effect: &Effect) -> bool {
        self == Self::PromptsAndMetadata
            && matches!(effect, Effect::Identify { .. } | Effect::Badges { .. })
    }
}

pub struct StrictSpace {
    mux: FakeMux,
    takes: Takes,
}

impl StrictSpace {
    pub fn new(mux: FakeMux, takes: Takes) -> Self {
        Self {
            mux: mux.strict_reads(),
            takes,
        }
    }
}

impl Deref for StrictSpace {
    type Target = FakeMux;

    fn deref(&self) -> &FakeMux {
        &self.mux
    }
}

impl Drop for StrictSpace {
    fn drop(&mut self) {
        if std::thread::panicking() {
            return;
        }
        for sent in self.mux.sent() {
            assert!(
                self.takes.input(&sent.input),
                "the space takes {:?}, not this input: {sent:?}",
                self.takes
            );
        }
        for effect in self.mux.effects() {
            assert!(
                self.takes.effect(&effect),
                "the space takes {:?}, not this effect: {effect:?}",
                self.takes
            );
        }
    }
}
