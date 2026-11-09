//! Whether a running Codex session can still run Namedrop's hooks.
//!
//! Codex runs a plugin's hooks from its own copy and refuses them once that copy
//! differs from what the session approved. Installing a new version removes the
//! old copy. A refused hook can report nothing, so every hook that does run records
//! the copy it ran from. A session whose copy is gone is one Codex is refusing. A
//! session with no record has run no hook yet, and may only be starting.

use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const FORGET_AFTER: Duration = Duration::from_secs(30 * 24 * 60 * 60);

pub(crate) const REFUSED_REASON: &str = "Codex refused this session's Namedrop hooks because the plugin changed after the session started. Start a new Codex session in that pane and trust the hooks at Hooks need review";

fn directory(state: &Path) -> PathBuf {
    state.join("hook-runs")
}

fn path(state: &Path, session: &str) -> PathBuf {
    directory(state).join(crate::lower_hex(&Sha256::digest(session)))
}

pub fn record_hook_run(input: &str, state: &Path, plugin_root: Option<&Path>) {
    let session = serde_json::from_str::<serde_json::Value>(input)
        .ok()
        .and_then(|input| input["session_id"].as_str().map(str::to_owned))
        .filter(|session| !session.is_empty());
    let (Some(session), Some(root)) = (session, plugin_root.and_then(Path::to_str)) else {
        return;
    };
    let record = path(state, &session);
    if fs::read_to_string(&record).is_ok_and(|recorded| recorded == root) {
        // Keeps a session in use from aging out of forget_old.
        let _ = fs::File::options()
            .write(true)
            .open(&record)
            .and_then(|file| file.set_modified(SystemTime::now()));
        return;
    }
    forget_old(state);
    let _ = fs::create_dir_all(directory(state));
    let _ = fs::write(record, root);
}

pub(crate) fn refused(state: &Path, session: &str) -> bool {
    fs::read_to_string(path(state, session))
        .is_ok_and(|root| !root.is_empty() && !Path::new(&root).is_dir())
}

fn forget_old(state: &Path) {
    let Ok(entries) = fs::read_dir(directory(state)) else {
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
