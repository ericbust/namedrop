//! Where a Claude Code session stands between its turns, as its own hooks saw
//! it. Herdr reads a session that finished its turn while background agents run
//! as working, so delivery asks its hooks instead.
//!
//! Each edge has its own file, written only by the hook that sees it, so a
//! late Stop worker never overwrites a newer prompt: the newer time wins.

use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TurnPhase {
    /// A prompt started a turn, and no Stop has ended it since.
    Prompted { at_ms: u64 },
    /// The last turn ended, and no prompt has come since.
    Stopped { at_ms: u64 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TurnEdge {
    Prompt,
    Stop,
}

impl TurnEdge {
    fn extension(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Stop => "stop",
        }
    }
}

fn edge_path(state: &Path, socket: &str, session: &str, edge: TurnEdge) -> PathBuf {
    let key = serde_json::to_vec(&(socket, session)).unwrap_or_default();
    state.join("turn-phase").join(format!(
        "{}.{}",
        crate::lower_hex(&Sha256::digest(key)),
        edge.extension()
    ))
}

fn edge_at(state: &Path, socket: &str, session: &str, edge: TurnEdge) -> Option<u64> {
    let mut text = String::new();
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(edge_path(state, socket, session, edge))
        .ok()?
        .take(32)
        .read_to_string(&mut text)
        .ok()?;
    text.trim().parse().ok()
}

/// Records that `session` passed `edge` at `at_ms`, unless a later one of the
/// same edge is already recorded. A failed write leaves the older record,
/// which delivery reads as the session's last known phase.
pub fn record(state: &Path, socket: &str, session: &str, edge: TurnEdge, at_ms: u64) {
    let recorded = edge_at(state, socket, session, edge);
    if recorded.is_some_and(|recorded| recorded >= at_ms) {
        return;
    }
    let path = edge_path(state, socket, session, edge);
    let Some(directory) = path.parent() else {
        return;
    };
    if fs::create_dir_all(directory).is_err() {
        return;
    }
    if recorded.is_none() {
        forget_old(directory);
    }
    let next = path.with_extension(format!("{}.next.{}", edge.extension(), std::process::id()));
    let written = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&next)
        .and_then(|mut file| file.write_all(at_ms.to_string().as_bytes()));
    if written.is_ok() {
        let _ = fs::rename(&next, path);
    } else {
        let _ = fs::remove_file(next);
    }
}

/// A session that recorded no edge for this long has most likely ended. If it
/// has not, delivery reads it the way Herdr does until its next edge.
const FORGET_AFTER: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Removes the records of sessions gone quiet, once per new session.
fn forget_old(directory: &Path) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        if entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| modified.elapsed().is_ok_and(|age| age > FORGET_AFTER))
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// The session's phase, from the later of its recorded edges. A prompt and a
/// Stop at the same millisecond read as a turn still running.
pub fn read(state: &Path, socket: &str, session: &str) -> Option<TurnPhase> {
    let stopped = edge_at(state, socket, session, TurnEdge::Stop);
    let prompted = edge_at(state, socket, session, TurnEdge::Prompt);
    match (stopped, prompted) {
        (Some(stop), Some(prompt)) if stop > prompt => Some(TurnPhase::Stopped { at_ms: stop }),
        (Some(stop), None) => Some(TurnPhase::Stopped { at_ms: stop }),
        (_, Some(prompt)) => Some(TurnPhase::Prompted { at_ms: prompt }),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "namedrop-turn-phase-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn the_later_edge_decides_the_phase_whatever_order_the_hooks_wrote_in() {
        let state = state();
        assert_eq!(read(&state, "socket", "session"), None);
        record(&state, "socket", "session", TurnEdge::Prompt, 100);
        assert_eq!(
            read(&state, "socket", "session"),
            Some(TurnPhase::Prompted { at_ms: 100 })
        );
        record(&state, "socket", "session", TurnEdge::Stop, 200);
        assert_eq!(
            read(&state, "socket", "session"),
            Some(TurnPhase::Stopped { at_ms: 200 })
        );
        // A task notice opened a turn while the Stop's worker was still
        // checking the other Stop hooks; the worker's late write of an older
        // Stop must not reopen the gate.
        record(&state, "socket", "session", TurnEdge::Prompt, 300);
        record(&state, "socket", "session", TurnEdge::Stop, 250);
        assert_eq!(
            read(&state, "socket", "session"),
            Some(TurnPhase::Prompted { at_ms: 300 })
        );
        record(&state, "socket", "session", TurnEdge::Stop, 300);
        assert_eq!(
            read(&state, "socket", "session"),
            Some(TurnPhase::Prompted { at_ms: 300 })
        );
        assert_eq!(read(&state, "other-socket", "session"), None);
        assert_eq!(read(&state, "socket", "other-session"), None);
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn a_new_session_forgets_sessions_quiet_for_a_week() {
        let state = state();
        record(&state, "socket", "old", TurnEdge::Stop, 100);
        record(&state, "socket", "recent", TurnEdge::Stop, 100);
        let week_ago = std::time::SystemTime::now() - FORGET_AFTER - Duration::from_secs(60);
        fs::File::options()
            .write(true)
            .open(edge_path(&state, "socket", "old", TurnEdge::Stop))
            .unwrap()
            .set_modified(week_ago)
            .unwrap();
        record(&state, "socket", "new", TurnEdge::Prompt, 200);
        assert_eq!(read(&state, "socket", "old"), None);
        assert_eq!(
            read(&state, "socket", "recent"),
            Some(TurnPhase::Stopped { at_ms: 100 })
        );
        fs::remove_dir_all(state).unwrap();
    }
}
