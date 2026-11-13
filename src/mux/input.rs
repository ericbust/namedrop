use super::{AgentIdentity, Key};

#[derive(Clone, Copy, Debug)]
pub struct PromptRequest<'a> {
    /// The agent the caller verified under the pane lock.
    pub expect: &'a AgentIdentity,
    pub text: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Recipient {
    /// The agent's prompt box.
    Agent,
    /// The raw terminal.
    Pane,
}

#[derive(Clone, Copy, Debug)]
pub enum InputKind<'a> {
    Prompt(PromptRequest<'a>),
    /// A line typed and submitted into a shell.
    Line(&'a str),
    Keys {
        recipient: Recipient,
        keys: &'a [Key],
    },
}

/// Each failure keeps the multiplexer's own words, so a caller that reports
/// one says what it said before.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InputOutcome {
    Delivered,
    /// Nothing was typed, so the input can be queued again.
    Declined(Retry, String),
    /// Nothing was typed, and the same input cannot succeed later.
    Refused(String),
    /// The input may have reached the pane. It is never sent again.
    Ambiguous(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Retry {
    /// A status event will wake the drain.
    OnEvent,
    /// No event will come, so retry on a timer.
    AfterDelay,
}
