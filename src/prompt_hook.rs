//! The turn start a Claude Code prompt records.

use namedrop::{
    Clock, SystemClock,
    turn_phase::{self, TurnEdge},
};
use serde::Deserialize;
use std::path::Path;

#[derive(Deserialize)]
struct PromptInput {
    hook_event_name: String,
    session_id: Option<String>,
    prompt_id: Option<String>,
    turn_id: Option<String>,
    prompt: Option<String>,
}

/// A Claude Code prompt whose turn start this hook recorded.
pub struct Prompted {
    socket: String,
    session: String,
    at_ms: u64,
    prompt: String,
}

/// Records that a Claude Code prompt starts a turn, from the hook's input and
/// its home's endpoint, before anything asks Herdr: a Herdr error must not leave
/// the last Stop newest while the turn runs. A Claude Code prompt carries a
/// prompt id and no turn id. A prompt another plugin's hook blocks or stops
/// still counts as a started turn, so its mail waits for the next Stop or for
/// the background agents.
pub fn record(state: &Path, endpoint: Option<&str>, input: &str) -> Option<Prompted> {
    let input: PromptInput = serde_json::from_str(input).ok()?;
    if input.hook_event_name != "UserPromptSubmit"
        || input.prompt_id.is_none()
        || input.turn_id.is_some()
    {
        return None;
    }
    let prompted = Prompted {
        socket: endpoint?.to_owned(),
        session: input.session_id.filter(|id| !id.is_empty())?,
        at_ms: SystemClock.now_unix_ms(),
        prompt: input.prompt.unwrap_or_default(),
    };
    turn_phase::record(
        state,
        &prompted.socket,
        &prompted.session,
        TurnEdge::Prompt,
        prompted.at_ms,
    );
    Some(prompted)
}

impl Prompted {
    /// Namedrop's own hook blocked the prompt, so it started no turn, and
    /// Claude Code puts its text back into the prompt box.
    pub fn blocked(&self, state: &Path) {
        turn_phase::record(
            state,
            &self.socket,
            &self.session,
            TurnEdge::Blocked,
            self.at_ms,
        );
        turn_phase::record_restored(state, &self.socket, &self.session, self.at_ms, &self.prompt);
    }
}
