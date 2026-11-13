//! Session bindings: which agent session runs in a Rex block, as the
//! block's own Namedrop hook recorded it. Rex reports no session, so this file
//! is the only witness. Commit 17 adds the writer.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct Binding {
    /// `block:<uuid>`.
    pub block: String,
    /// The canonical endpoint, for a sandboxed deferral.
    pub endpoint: String,
    /// `session:<uuid>`, for a sandboxed deferral.
    pub space: String,
    /// The block's foreground when the hook ran, as `<pid>@<start>`.
    pub foreground: String,
    /// The hook's agent, as `<pid>@<start>`. Equal to `foreground` when the
    /// agent is the foreground itself, and otherwise behind a launcher.
    pub agent: String,
    /// The wrapper's runtime, which the record's `app` must equal.
    pub runtime: String,
    /// The hook's `session_id`.
    pub session: String,
    pub bound_at_ms: u64,
}

impl Binding {
    /// The pid of the bound agent, when it is not the foreground and so
    /// needs a read of its own.
    pub(super) fn launched_agent(&self) -> Option<u32> {
        if self.agent == self.foreground {
            return None;
        }
        self.agent.split_once('@')?.0.parse().ok()
    }
}

/// The bindings under `<state>/rex/sessions/`, one file per block.
#[derive(Clone, Debug)]
pub(super) struct Bindings {
    sessions: PathBuf,
}

impl Bindings {
    pub(super) fn new(state: &Path) -> Self {
        Self {
            sessions: state.join("rex").join("sessions"),
        }
    }

    /// `<uuid>.json` for `block:<uuid>`. Block IDs come from the server, so
    /// one that is not a UUID names no file.
    pub(super) fn path(&self, block: &str) -> Option<PathBuf> {
        let uuid = block.strip_prefix("block:")?;
        (!uuid.is_empty() && uuid.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
            .then(|| self.sessions.join(format!("{uuid}.json")))
    }

    /// The block's binding. A missing, unreadable, or malformed file, or one
    /// that names another block, is no binding.
    pub(super) fn read(&self, block: &str) -> Option<Binding> {
        let text = std::fs::read(self.path(block)?).ok()?;
        serde_json::from_slice::<Binding>(&text)
            .ok()
            .filter(|binding| binding.block == block)
    }
}
