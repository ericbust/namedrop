//! Whether a running Codex session still runs Namedrop's hooks.
//!
//! Codex fingerprints a plugin each time it runs one of its hooks, and refuses the
//! hook when the result differs from what that session approved. A refused hook
//! can report nothing, so every hook that does run records the package revision it
//! ran under. A session on another revision is one Codex is refusing. A session
//! with no record has run no hook yet, and may only be starting.

use crate::AgentRuntime;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const FORGET_AFTER: Duration = Duration::from_secs(30 * 24 * 60 * 60);

pub(crate) const REFUSED_REASON: &str = "Codex refused this session's Namedrop hooks because the plugin changed after the session started. Start a new Codex session in that pane and trust the hooks at Hooks need review";

#[derive(Clone, Copy)]
pub(crate) struct HookRuns<'a> {
    pub(crate) state: &'a Path,
    pub(crate) home: Option<&'a Path>,
}

impl HookRuns<'_> {
    fn directory(&self) -> PathBuf {
        self.state.join("hook-runs")
    }

    fn path(&self, session: &str) -> PathBuf {
        self.directory()
            .join(crate::lower_hex(&Sha256::digest(session)))
    }

    fn package_revision(&self) -> Option<String> {
        let manifest = crate::package_directory(self.home?, AgentRuntime::Codex)
            .join(".codex-plugin/plugin.json");
        let manifest: serde_json::Value = serde_json::from_slice(&fs::read(manifest).ok()?).ok()?;
        Some(manifest.get("version")?.as_str()?.to_owned())
    }

    pub(crate) fn record(&self, session: &str) {
        let Some(revision) = self.package_revision() else {
            return;
        };
        let path = self.path(session);
        if fs::read_to_string(&path).is_ok_and(|recorded| recorded == revision) {
            return;
        }
        self.forget_old();
        let _ = fs::create_dir_all(self.directory());
        let _ = fs::write(path, revision);
    }

    pub(crate) fn refused(&self, session: &str) -> bool {
        let Some(revision) = self.package_revision() else {
            return false;
        };
        fs::read_to_string(self.path(session)).is_ok_and(|recorded| recorded != revision)
    }

    fn forget_old(&self) {
        let Ok(entries) = fs::read_dir(self.directory()) else {
            return;
        };
        for entry in entries.flatten() {
            let old = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| SystemTime::now().duration_since(modified).ok())
                .is_some_and(|age| age > FORGET_AFTER);
            if old {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}
