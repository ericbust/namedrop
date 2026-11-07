//! What each host session was last shown of the workspace roster, so a turn
//! carries a part only when it changed. Each session keeps one small file of
//! digests under the plugin state directory, outside the state store, so a
//! prompt never takes the store's write lock. Losing a file only makes the next
//! turn show the whole roster, so every failure here shows rather than hides.

use crate::persona::{NO_OTHER_WORKSPACES, Roster, RosterParts};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, FileExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

const DIRECTORY: &str = "roster-shown";

/// A session quiet this long is shown its whole roster again. Pruning by age
/// bounds how many files the directory holds.
const KEPT_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);

const MAX_BYTES: u64 = 4096;

/// Digests of the last parts shown. `others` is `None` when the last roster
/// shown had no other-workspaces line, or nothing was shown yet.
#[derive(Default, PartialEq, Deserialize, Serialize)]
struct Shown {
    rows: Option<String>,
    others: Option<String>,
}

impl Shown {
    fn of(roster: &Roster) -> Self {
        Self {
            rows: Some(digest(&roster.compared_rows)),
            others: roster.others.as_deref().map(digest),
        }
    }
}

fn digest(text: &str) -> String {
    crate::lower_hex(&Sha256::digest(text.as_bytes()))
}

/// The parts of `roster` the session was not last shown, recorded as shown,
/// so the caller must carry every part returned. With no session, or while
/// another hook of the same session holds its memory, the whole roster is
/// returned and nothing is recorded.
pub(crate) fn unseen(state: &Path, session: Option<&str>, roster: &Roster) -> RosterParts {
    let Some(file) = session.and_then(|session| lock(&path(state, session), Lock::Record)) else {
        return RosterParts::whole(roster);
    };
    let last = read(&file);
    let shown = Shown::of(roster);
    let parts = RosterParts {
        rows: (shown.rows != last.rows).then(|| roster.rows.clone()),
        others: (shown.others != last.others).then(|| {
            roster
                .others
                .clone()
                .unwrap_or_else(|| NO_OTHER_WORKSPACES.to_owned())
        }),
    };
    if shown != last {
        write(&file, &shown);
    }
    parts
}

/// Forgets what the session was shown, so its next turn carries the whole
/// roster. It waits for a prompt hook holding the memory and empties the file
/// in place, so a write that raced it can never restore what it forgot.
pub(crate) fn forget(state: &Path, session: &str) {
    if let Some(file) = lock(&path(state, session), Lock::Forget) {
        let _ = file.set_len(0);
    }
}

#[derive(Deserialize)]
struct SessionStartInput {
    hook_event_name: String,
    session_id: Option<String>,
}

/// Any SessionStart (startup, resume, clear, compaction, fork) gives the host
/// a context that may no longer hold the roster, so the session's next turn
/// shows all of it. It needs only the state directory, because under Codex's
/// shared app-server the hook's runtime usually cannot be built.
pub fn forget_shown_roster(input: &str, state: &Path) {
    let Ok(input) = serde_json::from_str::<SessionStartInput>(input) else {
        return;
    };
    if input.hook_event_name != "SessionStart" {
        return;
    }
    if let Some(session) = input.session_id.filter(|id| !id.is_empty()) {
        forget(state, &session);
    }
    prune(state);
}

fn path(state: &Path, session: &str) -> PathBuf {
    state.join(DIRECTORY).join(digest(session))
}

#[derive(Clone, Copy, PartialEq)]
enum Lock {
    /// Creates the file and never waits: a contended prompt shows everything.
    Record,
    /// Never creates the file and waits for its holder.
    Forget,
}

fn lock(path: &Path, mode: Lock) -> Option<File> {
    if mode == Lock::Record {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path.parent()?)
            .ok()?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(mode == Lock::Record)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let operation = match mode {
        Lock::Record => libc::LOCK_EX | libc::LOCK_NB,
        Lock::Forget => libc::LOCK_EX,
    };
    (unsafe { libc::flock(file.as_raw_fd(), operation) } == 0).then_some(file)
}

fn read(file: &File) -> Shown {
    let mut bytes = Vec::new();
    match file.take(MAX_BYTES).read_to_end(&mut bytes) {
        Ok(_) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => Shown::default(),
    }
}

fn write(file: &File, shown: &Shown) {
    if let Ok(bytes) = serde_json::to_vec(shown) {
        let _ = file.set_len(0).and_then(|()| file.write_all_at(&bytes, 0));
    }
}

/// Removing a file only ever makes a session see its whole roster again, so
/// this needs no lock.
fn prune(state: &Path) {
    let Ok(entries) = fs::read_dir(state.join(DIRECTORY)) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| now.duration_since(modified).is_ok_and(|age| age > KEPT_FOR));
        if stale {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_STATE: AtomicU64 = AtomicU64::new(1);

    struct State(PathBuf);
    impl State {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "namedrop-shown-roster-{}-{}",
                std::process::id(),
                NEXT_STATE.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for State {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn roster() -> Roster {
        Roster {
            rows: "Namedrop workspace roster. Rows:\n- @lead (you) — Generic · busy\n".to_owned(),
            compared_rows: "Namedrop workspace roster. Rows:\n- @lead (you) — Generic\n".to_owned(),
            others: Some("Other workspaces: @scout.".to_owned()),
        }
    }

    fn shown_whole(state: &State) -> bool {
        let parts = unseen(&state.0, Some("session-1"), &roster());
        match (parts.rows, parts.others) {
            (Some(_), Some(_)) => true,
            (None, None) => false,
            parts => panic!("only part of an unchanged roster came again: {parts:?}"),
        }
    }

    #[test]
    fn a_contended_prompt_shows_the_whole_roster_and_records_nothing() {
        let state = State::new();
        let held = lock(&path(&state.0, "session-1"), Lock::Record).unwrap();
        assert!(shown_whole(&state));
        drop(held);
        assert!(shown_whole(&state), "the contended prompt recorded");
        assert!(!shown_whole(&state));
    }

    #[test]
    fn a_reset_waits_for_a_racing_write_and_is_never_undone_by_it() {
        let state = State::new();
        let held = lock(&path(&state.0, "session-1"), Lock::Record).unwrap();
        let reset = std::thread::spawn({
            let state = state.0.clone();
            move || forget(&state, "session-1")
        });
        std::thread::sleep(Duration::from_millis(100));
        write(&held, &Shown::of(&roster()));
        drop(held);
        reset.join().unwrap();
        assert!(shown_whole(&state), "the racing write restored the memory");
    }

    #[test]
    fn a_session_start_prunes_memories_left_idle() {
        let state = State::new();
        assert!(shown_whole(&state));
        let idle = SystemTime::now() - KEPT_FOR - Duration::from_secs(60);
        File::options()
            .write(true)
            .open(path(&state.0, "session-1"))
            .unwrap()
            .set_modified(idle)
            .unwrap();
        forget_shown_roster(
            r#"{"hook_event_name":"SessionStart","source":"startup","session_id":"session-2"}"#,
            &state.0,
        );
        assert!(!path(&state.0, "session-1").exists());
        assert!(shown_whole(&state));
    }
}
