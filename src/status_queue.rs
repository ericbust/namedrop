//! A space's status queue: the markers its agent-status worker serves, how
//! one is queued and the worker started, and what the worker leaves for
//! Doctor. The worker runs detached with its output discarded, so the event
//! it is failing on, and any event it had to set aside, is written beside the
//! queue it serves.

use crate::{
    DraftHold, SpaceId,
    daemon::{self, Scope, Starter},
    locator::{Locator, Name},
    mux::{Endpoint, MuxEvent},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Child, Command},
    time::{Duration, SystemTime, UNIX_EPOCH},
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

/// What one marker in a space's status queue asks its worker to do, in the
/// shape its file holds.
#[derive(Clone, Debug, PartialEq)]
pub enum Marker {
    /// `{"operation":"mailbox-wake"}`: drain the space's mailboxes.
    Wake,
    /// `{"operation":"draft-recheck","held":[..]}`: read the drafts that held
    /// mail again.
    DraftRecheck(Vec<DraftHold>),
    /// `{"operation":"mux-event","event":{..}}`: one event from the Rex
    /// bridge.
    Event(MuxEvent),
    /// Herdr's status event data as its hook received it, which the worker
    /// decodes, so a Herdr event the hook cannot read still reaches it.
    Herdr(Value),
}

impl Marker {
    /// The marker a file's JSON holds. Any shape the worker does not name is
    /// Herdr's.
    pub fn parse(input: Value) -> Result<Self, String> {
        if input == json!({"operation": "mailbox-wake"}) {
            return Ok(Self::Wake);
        }
        let invalid = |error: serde_json::Error| format!("invalid operation input: {error}");
        match input["operation"].as_str() {
            Some("draft-recheck") => serde_json::from_value(input["held"].clone())
                .map(Self::DraftRecheck)
                .map_err(invalid),
            Some("mux-event") => serde_json::from_value(input["event"].clone())
                .map(Self::Event)
                .map_err(invalid),
            _ => Ok(Self::Herdr(input)),
        }
    }

    pub fn to_value(&self) -> Value {
        match self {
            Self::Wake => json!({"operation": "mailbox-wake"}),
            Self::DraftRecheck(held) => json!({"operation": "draft-recheck", "held": held}),
            Self::Event(event) => json!({"operation": "mux-event", "event": event}),
            Self::Herdr(data) => data.clone(),
        }
    }
}

/// Queues `marker` for the worker of `space`: written whole as `queued-*`,
/// synced, and renamed to `pending-*`, so a worker never reads half of one.
pub fn enqueue(
    state: &Path,
    endpoint: &Endpoint,
    space: &SpaceId,
    marker: &Marker,
) -> io::Result<()> {
    let directory = directory(state, &endpoint.key, space.as_str());
    fs::create_dir_all(&directory)?;
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700))?;
    let occurred_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let marker_id = format!("{}-{occurred_at}", std::process::id());
    let queued = directory.join(format!("queued-{marker_id}"));
    let pending = directory.join(format!("pending-{marker_id}"));
    let bytes = serde_json::to_vec(&marker.to_value())?;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&queued)?;
    if let Err(error) = file
        .write_all(&bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&queued, pending))
        .and_then(|()| fs::File::open(&directory).and_then(|file| file.sync_all()))
    {
        let _ = fs::remove_file(queued);
        return Err(error);
    }
    Ok(())
}

/// Starts the worker of `space`. A worker already serving the queue holds
/// its lock, and this one exits at once.
pub fn spawn_worker(state: &Path, endpoint: &Endpoint, space: &SpaceId) -> Result<Child, String> {
    worker_command(state, endpoint, space)?
        .spawn()
        .map_err(|error| error.to_string())
}

/// The worker reconciles a whole space and outlives the process that started
/// it, so it is nobody's pane and gets only its space's location.
fn worker_command(state: &Path, endpoint: &Endpoint, space: &SpaceId) -> Result<Command, String> {
    let mut command = daemon::command(
        &crate::runtime_executable()?,
        Scope::Space(endpoint, space),
        Starter::Caller,
    );
    command
        .args(["hook", "agent-status-worker"])
        .env("NAMEDROP_HERDR_TIMEOUT_MS", "10000");
    Locator::StateDir.pass(&mut command, Name::of(endpoint.kind), state);
    Ok(command)
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
    use std::collections::BTreeMap;

    fn location(command: &Command) -> BTreeMap<String, Option<String>> {
        command
            .get_envs()
            .filter(|(key, _)| {
                let key = key.to_string_lossy();
                key.starts_with("HERDR_") || key.starts_with("NAMEDROP_")
            })
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.map(|value| value.to_string_lossy().into_owned()),
                )
            })
            .collect()
    }

    /// The worker gets its state directory, endpoint, and space under the
    /// names of its endpoint's kind, the CLI's Herdr timeout, and no pane. It
    /// keeps the Rex timeout its starter holds.
    #[test]
    fn the_status_worker_gets_its_space_under_its_endpoint_s_names() {
        let state = Path::new("/state");
        let herdr =
            worker_command(state, &Endpoint::parse("/run/h.sock"), &SpaceId::new("w1")).unwrap();
        assert_eq!(
            location(&herdr),
            BTreeMap::from(
                [
                    ("HERDR_PANE_ID", None),
                    ("HERDR_PLUGIN_STATE_DIR", Some("/state")),
                    ("HERDR_SOCKET_PATH", Some("/run/h.sock")),
                    ("HERDR_WORKSPACE_ID", Some("w1")),
                    ("NAMEDROP_ENDPOINT", None),
                    ("NAMEDROP_HERDR_TIMEOUT_MS", Some("10000")),
                    ("NAMEDROP_HOOK_RUNTIME", None),
                    ("NAMEDROP_REX_BRIDGE_WORKERS", None),
                    ("NAMEDROP_PANE_ID", None),
                    ("NAMEDROP_SPACE_ID", None),
                    ("NAMEDROP_STATE_DIR", None),
                ]
                .map(|(key, value)| (key.to_owned(), value.map(str::to_owned)))
            )
        );
        let rex = worker_command(
            state,
            &Endpoint::parse("rex:/tmp/r.sock"),
            &SpaceId::new("s1"),
        )
        .unwrap();
        assert_eq!(
            location(&rex),
            BTreeMap::from(
                [
                    ("HERDR_PANE_ID", None),
                    ("HERDR_PLUGIN_STATE_DIR", None),
                    ("HERDR_SOCKET_PATH", None),
                    ("HERDR_WORKSPACE_ID", None),
                    ("NAMEDROP_ENDPOINT", Some("rex:/tmp/r.sock")),
                    ("NAMEDROP_HERDR_TIMEOUT_MS", Some("10000")),
                    ("NAMEDROP_HOOK_RUNTIME", None),
                    ("NAMEDROP_REX_BRIDGE_WORKERS", None),
                    ("NAMEDROP_PANE_ID", None),
                    ("NAMEDROP_SPACE_ID", Some("s1")),
                    ("NAMEDROP_STATE_DIR", Some("/state")),
                ]
                .map(|(key, value)| (key.to_owned(), value.map(str::to_owned)))
            )
        );
    }

    /// Each shape a marker file holds reads back as the marker that wrote
    /// it, and Herdr's data and today's wake are written byte for byte as
    /// before.
    #[test]
    fn every_marker_round_trips_through_its_file_shape() {
        let slot = crate::mux::PaneSlot {
            space: SpaceId::new("session:s"),
            pane: crate::mux::PaneId::new("block:b"),
            tab: None,
        };
        let herdr = json!({"type": "pane_agent_status_changed", "pane_id": "w1:p1"});
        let markers = [
            Marker::Wake,
            Marker::DraftRecheck(vec![DraftHold {
                address_id: crate::PaneAddressId::new("address-1"),
                pane_id: "w1:p1".to_owned(),
                agent: crate::AgentRuntime::ClaudeCode,
                session: Some("session-1".to_owned()),
            }]),
            Marker::Event(MuxEvent::PaneClosed(slot.clone())),
            Marker::Event(MuxEvent::TabClosed {
                space: SpaceId::new("session:s"),
                tab: crate::mux::TabId::new("window:w"),
            }),
            Marker::Event(MuxEvent::AgentStatusChanged(slot)),
            Marker::Event(MuxEvent::Resync(SpaceId::new("session:s"))),
            Marker::Event(MuxEvent::SpaceClosed(SpaceId::new("session:s"))),
            Marker::Herdr(herdr.clone()),
        ];
        let state = std::env::temp_dir().join(format!(
            "namedrop-status-queue-markers-{}",
            std::process::id()
        ));
        let endpoint = Endpoint::parse("rex:/tmp/r.sock");
        let space = SpaceId::new("session:s");
        for marker in &markers {
            enqueue(&state, &endpoint, &space, marker).unwrap();
        }
        let queue = directory(&state, &endpoint.key, space.as_str());
        let mut files: Vec<_> = fs::read_dir(&queue)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        files.sort();
        let read: Vec<Marker> = files
            .iter()
            .map(|file| Marker::parse(serde_json::from_slice(&fs::read(file).unwrap()).unwrap()))
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(read, markers);
        assert_eq!(
            fs::read(&files[0]).unwrap(),
            br#"{"operation":"mailbox-wake"}"#
        );
        assert_eq!(
            fs::read(files.last().unwrap()).unwrap(),
            serde_json::to_vec(&herdr).unwrap()
        );
        assert_eq!(
            Marker::Event(MuxEvent::SpaceClosed(SpaceId::new("session:s"))).to_value(),
            json!({"operation": "mux-event", "event": {"space_closed": "session:s"}})
        );
        assert!(
            Marker::parse(json!({"operation": "mux-event", "event": {"moved": 1}}))
                .unwrap_err()
                .starts_with("invalid operation input: ")
        );
        let _ = fs::remove_dir_all(state);
    }

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
