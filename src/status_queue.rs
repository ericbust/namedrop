//! What the agent-status worker leaves for Doctor. The worker runs detached
//! with its output discarded, so the event it is failing on, and any event it
//! had to set aside, is written beside the queue it serves.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

const FAILING: &str = "failing.json";
const SET_ASIDE: &str = "set-aside.json";

/// An event keeps the queue waiting this long, and at least `MIN_ATTEMPTS`
/// tries, before it is set aside. A later status event inspects the same
/// current state, so a set-aside event loses nothing a later one cannot redo.
pub const GIVE_UP_AFTER: Duration = Duration::from_secs(10 * 60);
const MIN_ATTEMPTS: u32 = 5;
/// The longest a worker spends between two attempts: its backoff, a Herdr
/// timeout and a short gate wait. A longer gap, such as a laptop asleep, counts
/// as this much retrying.
const LONGEST_RETRY_GAP_MS: u64 = 30_000;
/// A failure not retried for this long has no worker behind it any more.
pub const STALE_AFTER_MS: u64 = 2 * 60_000;
const ERROR_LIMIT: usize = 600;

pub fn directory(state: &Path, socket_id: &str, space_id: &str) -> PathBuf {
    let scope = crate::lower_hex(&Sha256::digest(format!("{socket_id}\0{space_id}")));
    state.join("agent-status-reconciliation").join(scope)
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Failure {
    pub marker: String,
    pub event: Option<String>,
    pub pane_id: Option<String>,
    pub error: String,
    pub attempts: u32,
    pub first_failed_ms: u64,
    pub last_failed_ms: u64,
    /// Time spent retrying, which is what giving up is measured by.
    #[serde(default)]
    pub retrying_ms: u64,
    /// The worker retrying it, so Doctor can tell when nobody is.
    #[serde(default)]
    pub worker_pid: u32,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SetAside {
    #[serde(flatten)]
    pub failure: Failure,
    pub set_aside_ms: u64,
}

fn read<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn write(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, serde_json::to_vec(value)?)?;
    fs::rename(temporary, path)
}

fn name(marker: &Path) -> String {
    marker
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Counts one more failure of `marker`, continuing its record across worker
/// restarts, and returns the record.
pub fn record_failure(
    directory: &Path,
    marker: &Path,
    input: &serde_json::Value,
    error: &str,
    now_ms: u64,
) -> Failure {
    let path = directory.join(FAILING);
    let previous = read::<Failure>(&path).filter(|failure| failure.marker == name(marker));
    let failure = Failure {
        marker: name(marker),
        event: input["type"].as_str().map(str::to_owned),
        pane_id: input["pane_id"].as_str().map(str::to_owned),
        error: error.chars().take(ERROR_LIMIT).collect(),
        attempts: previous.as_ref().map_or(0, |failure| failure.attempts) + 1,
        first_failed_ms: previous
            .as_ref()
            .map_or(now_ms, |failure| failure.first_failed_ms),
        last_failed_ms: now_ms,
        retrying_ms: previous.as_ref().map_or(0, |failure| {
            failure.retrying_ms
                + now_ms
                    .saturating_sub(failure.last_failed_ms)
                    .min(LONGEST_RETRY_GAP_MS)
        }),
        worker_pid: std::process::id(),
    };
    let _ = write(&path, &failure);
    failure
}

/// The event behind `marker` was handled or dropped, so its failure is over.
pub fn clear_failure(directory: &Path, marker: &Path) {
    let path = directory.join(FAILING);
    if read::<Failure>(&path).is_some_and(|failure| failure.marker == name(marker)) {
        let _ = fs::remove_file(path);
    }
}

pub fn should_set_aside(failure: &Failure, give_up_after: Duration) -> bool {
    failure.attempts >= MIN_ATTEMPTS && failure.retrying_ms >= give_up_after.as_millis() as u64
}

/// Whether a recorded failure is still being retried: its worker is alive
/// and tried again recently. After a reboot or a killed worker it is not.
pub fn still_retrying(failure: &Failure, now_ms: u64) -> bool {
    let alive = failure.worker_pid != 0
        && (unsafe { libc::kill(failure.worker_pid as libc::pid_t, 0) } == 0
            || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM));
    alive && now_ms.saturating_sub(failure.last_failed_ms) <= STALE_AFTER_MS
}

/// Removes a failure record nobody is retrying any more.
pub fn forget_stale(directory: &Path, now_ms: u64) {
    let path = directory.join(FAILING);
    if read::<Failure>(&path).is_some_and(|failure| !still_retrying(&failure, now_ms)) {
        let _ = fs::remove_file(path);
    }
}

/// Moves the event out of the queue as `rejected-<marker>` and records why.
pub fn set_aside(
    directory: &Path,
    marker: &Path,
    failure: &Failure,
    now_ms: u64,
) -> io::Result<()> {
    fs::rename(
        marker,
        marker.with_file_name(format!("rejected-{}", name(marker))),
    )?;
    write(
        &directory.join(SET_ASIDE),
        &SetAside {
            failure: failure.clone(),
            set_aside_ms: now_ms,
        },
    )?;
    let _ = fs::remove_file(directory.join(FAILING));
    Ok(())
}

/// What Doctor reports for one space's queue: the event it is failing on now,
/// and the last event it set aside.
pub fn health(directory: &Path) -> (Option<Failure>, Option<SetAside>) {
    (
        read(&directory.join(FAILING)),
        read(&directory.join(SET_ASIDE)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_counts_across_restarts_and_resets_for_another_event() {
        let directory =
            std::env::temp_dir().join(format!("namedrop-status-queue-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let event = serde_json::json!({"type":"pane_agent_detected","pane_id":"w5F:p1"});
        let first = directory.join("pending-1");
        record_failure(&directory, &first, &event, "No such file", 1_000);
        let again = record_failure(&directory, &first, &event, "No such file", 9_000);
        assert_eq!((again.attempts, again.first_failed_ms), (2, 1_000));
        assert_eq!(again.event.as_deref(), Some("pane_agent_detected"));
        assert_eq!(again.pane_id.as_deref(), Some("w5F:p1"));
        let other = record_failure(&directory, &directory.join("pending-2"), &event, "x", 9_500);
        assert_eq!((other.attempts, other.first_failed_ms), (1, 9_500));
        clear_failure(&directory, &first);
        assert!(
            health(&directory).0.is_some(),
            "another event's failure was cleared"
        );
        clear_failure(&directory, &directory.join("pending-2"));
        assert!(health(&directory).0.is_none());
        let _ = fs::remove_dir_all(directory);
    }

    /// Backoff tops out at a few seconds, so a long gap between attempts is a
    /// laptop asleep, not time spent retrying.
    #[test]
    fn time_asleep_between_attempts_does_not_count_toward_giving_up() {
        let directory = std::env::temp_dir().join(format!(
            "namedrop-status-queue-sleep-{}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        let marker = directory.join("pending-1");
        let event = serde_json::json!({"type":"pane_agent_status_changed"});
        let mut failure = record_failure(&directory, &marker, &event, "busy", 0);
        for at in [
            60 * 60_000,
            60 * 60_000 + 5_000,
            60 * 60_000 + 10_000,
            60 * 60_000 + 15_000,
        ] {
            failure = record_failure(&directory, &marker, &event, "busy", at);
        }
        assert_eq!(failure.attempts, 5);
        assert!(!should_set_aside(&failure, GIVE_UP_AFTER), "{failure:?}");
        let _ = fs::remove_dir_all(directory);
    }
}
