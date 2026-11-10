//! Where a Claude Code session stands between its turns, as its own hooks saw
//! it. Herdr reads a session that finished its turn while background agents run
//! as working, so delivery asks its hooks instead.
//!
//! Each edge has its own file, written only by the hook that sees it, and
//! each file only moves forward in time, so a late writer never overwrites a
//! newer edge: the newer time wins. A prompt a hook blocked started no turn,
//! so its prompt record also keeps the prompt it followed, which counts again
//! once the block is recorded.

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
    /// A prompt hook blocked or stopped the prompt recorded at this time, so
    /// it started no turn. Recorded with that prompt's time.
    Blocked,
}

impl TurnEdge {
    fn extension(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::Stop => "stop",
            Self::Blocked => "blocked",
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

/// The times an edge's file holds: when the edge was last passed, and for a
/// prompt, the prompt that started the turn before it.
fn edge_times(state: &Path, socket: &str, session: &str, edge: TurnEdge) -> Option<Vec<u64>> {
    let mut text = String::new();
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(edge_path(state, socket, session, edge))
        .ok()?
        .take(64)
        .read_to_string(&mut text)
        .ok()?;
    text.split_whitespace()
        .map(|time| time.parse().ok())
        .collect::<Option<Vec<u64>>>()
        .filter(|times| !times.is_empty())
}

fn edge_at(state: &Path, socket: &str, session: &str, edge: TurnEdge) -> Option<u64> {
    edge_times(state, socket, session, edge)?.first().copied()
}

/// The prompt that started the session's last turn: the last prompt, or the
/// one before it when a hook blocked the last.
fn turn_prompt(state: &Path, socket: &str, session: &str) -> Option<u64> {
    let times = edge_times(state, socket, session, TurnEdge::Prompt)?;
    if edge_at(state, socket, session, TurnEdge::Blocked) == Some(times[0]) {
        times.get(1).copied()
    } else {
        Some(times[0])
    }
}

/// Records that `session` passed `edge` at `at_ms`, unless a later one of the
/// same edge is already recorded. A failed write leaves the older record,
/// which delivery reads as the session's last known phase.
pub fn record(state: &Path, socket: &str, session: &str, edge: TurnEdge, at_ms: u64) {
    let recorded = edge_at(state, socket, session, edge);
    if recorded.is_some_and(|recorded| recorded >= at_ms) {
        return;
    }
    let text = match (edge, turn_prompt(state, socket, session)) {
        (TurnEdge::Prompt, Some(before)) => format!("{at_ms} {before}"),
        _ => at_ms.to_string(),
    };
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
        .and_then(|mut file| file.write_all(text.as_bytes()));
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

/// The session's phase, from the later of its last Stop and the prompt that
/// started its last turn. A prompt and a Stop at the same millisecond read as
/// a turn still running.
pub fn read(state: &Path, socket: &str, session: &str) -> Option<TurnPhase> {
    let stopped = edge_at(state, socket, session, TurnEdge::Stop);
    let prompted = turn_prompt(state, socket, session);
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
    fn a_blocked_prompt_leaves_the_phase_its_turn_had_whenever_the_block_is_recorded() {
        let state = state();
        let phase = || read(&state, "socket", "session");
        let edge = |edge, at_ms| record(&state, "socket", "session", edge, at_ms);
        // Between turns, a blocked prompt keeps the session between turns.
        edge(TurnEdge::Prompt, 100);
        edge(TurnEdge::Stop, 200);
        edge(TurnEdge::Prompt, 300);
        edge(TurnEdge::Blocked, 300);
        assert_eq!(phase(), Some(TurnPhase::Stopped { at_ms: 200 }));
        // A second blocked prompt, and the Stop worker of the earlier turn
        // writing only after both, change nothing.
        edge(TurnEdge::Prompt, 400);
        edge(TurnEdge::Blocked, 400);
        edge(TurnEdge::Stop, 150);
        assert_eq!(phase(), Some(TurnPhase::Stopped { at_ms: 200 }));
        // The next prompt starts a turn, and a block of an older prompt
        // recorded late does not undo it.
        edge(TurnEdge::Prompt, 500);
        edge(TurnEdge::Blocked, 400);
        assert_eq!(phase(), Some(TurnPhase::Prompted { at_ms: 500 }));
        // While that turn runs, a blocked prompt leaves it running.
        edge(TurnEdge::Prompt, 600);
        edge(TurnEdge::Blocked, 600);
        assert_eq!(phase(), Some(TurnPhase::Prompted { at_ms: 500 }));
        // Before its Stop is recorded, the turn's end and a blocked prompt
        // after it read as between turns.
        edge(TurnEdge::Prompt, 800);
        edge(TurnEdge::Blocked, 800);
        edge(TurnEdge::Stop, 700);
        assert_eq!(phase(), Some(TurnPhase::Stopped { at_ms: 700 }));
        fs::remove_dir_all(&state).unwrap();

        // A first prompt that was blocked leaves no phase.
        let state = self::state();
        record(&state, "socket", "session", TurnEdge::Prompt, 100);
        record(&state, "socket", "session", TurnEdge::Blocked, 100);
        assert_eq!(read(&state, "socket", "session"), None);
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
