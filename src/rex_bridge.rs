//! One bridge per Rex server. Rex runs no plugins, so this long-lived process
//! holds the server's event stream and turns its events into markers in each
//! space's status queue, whose worker handles them and then drains mail.

use crate::{
    SpaceId, StateStore,
    daemon::{self, Scope, Starter},
    locator::{Locator, Name},
    mux::{
        Endpoint, MuxEvent,
        rex::events::{Liveness, Next, Pending, RexEvents, Wake},
    },
    status_queue::{self, Marker},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::{Child, ExitCode},
    sync::OnceLock,
    time::{Duration, Instant},
};

/// Runs the bridge for `endpoint` until SIGTERM or SIGINT, until its
/// server has been unreachable for two minutes, or until its state
/// directory is deleted or replaced. A second bridge for the same endpoint
/// exits at once.
pub fn run(state: &Path, endpoint: &Endpoint) -> ExitCode {
    let Ok(Some(lock)) = Lock::take(state, endpoint) else {
        return ExitCode::SUCCESS;
    };
    let Some(socket) = endpoint.unix_socket() else {
        return ExitCode::SUCCESS;
    };
    let Some(stop) = stop_on_signals() else {
        return ExitCode::SUCCESS;
    };
    let server = Socket {
        path: socket.to_owned(),
        state: state.to_owned(),
    };
    bridge(
        state,
        endpoint,
        &server,
        stop,
        &lock,
        &mut |space| status_queue::spawn_worker(state, endpoint, space),
        &Away::REX,
    );
    ExitCode::SUCCESS
}

/// Starts the bridge for `endpoint` unless one holds its lock, and says
/// whether it started one. The Rex runtime builder calls it, so any
/// Namedrop command or hook in a Rex block starts its server's bridge.
pub fn ensure(state: &Path, endpoint: &Endpoint) -> Result<bool, String> {
    if Lock::take(state, endpoint)
        .map_err(|error| error.to_string())?
        .is_none()
    {
        return Ok(false);
    }
    let mut command = daemon::command(
        &crate::runtime_executable()?,
        Scope::Endpoint(endpoint),
        Starter::Caller,
    );
    command.args(["rex", "bridge", "--endpoint", &endpoint.key]);
    Locator::StateDir.pass(&mut command, Name::of(endpoint.kind), state);
    let mut bridge = command.spawn().map_err(|error| error.to_string())?;
    // A long-lived starter, such as a status worker, reaps the bridge when it
    // exits instead of keeping a zombie.
    std::thread::spawn(move || bridge.wait());
    Ok(true)
}

fn scope(endpoint: &Endpoint) -> String {
    crate::lower_hex(&Sha256::digest(endpoint.key.as_bytes()))
}

/// A bridge's hold on its endpoint: the locked file, and the path it was
/// opened at.
struct Lock {
    file: fs::File,
    path: PathBuf,
}

impl Lock {
    /// The lock, or `None` while another bridge holds it.
    fn take(state: &Path, endpoint: &Endpoint) -> io::Result<Option<Self>> {
        use std::os::unix::fs::OpenOptionsExt;
        let directory = state.join("rex-bridge");
        fs::create_dir_all(&directory)?;
        let path = directory.join(format!("{}.lock", scope(endpoint)));
        let file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)?;
        Ok(file.try_lock().is_ok().then_some(Self { file, path }))
    }

    /// Whether the path still names the locked file. Once the state
    /// directory is deleted or replaced, another bridge could lock the new
    /// path, so this one stops.
    fn held(&self) -> bool {
        use std::os::unix::fs::MetadataExt;
        match (fs::metadata(&self.path), self.file.metadata()) {
            (Ok(path), Ok(file)) => (path.dev(), path.ino()) == (file.dev(), file.ino()),
            _ => false,
        }
    }
}

/// The stop request SIGTERM and SIGINT make. A process has one.
fn stop_on_signals() -> Option<&'static Wake> {
    static STOP: OnceLock<Option<Wake>> = OnceLock::new();
    extern "C" fn on_signal(_: libc::c_int) {
        if let Some(Some(stop)) = STOP.get() {
            stop.wake();
        }
    }
    let stop = STOP.get_or_init(|| Wake::new().ok()).as_ref()?;
    let handler = on_signal as extern "C" fn(libc::c_int);
    for signal in [libc::SIGTERM, libc::SIGINT] {
        // SAFETY: the handler makes only async-signal-safe calls: an atomic
        // store and one `write(2)`.
        unsafe { libc::signal(signal, handler as libc::sighandler_t) };
    }
    Some(stop)
}

/// One connection's events, as `RexEvents` gives them. Tests script it.
trait Stream {
    fn host_id(&self) -> Option<&str>;
    fn next(&mut self, within: Duration) -> Next;
}

impl Stream for RexEvents<'_> {
    fn host_id(&self) -> Option<&str> {
        RexEvents::host_id(self)
    }

    fn next(&mut self, within: Duration) -> Next {
        RexEvents::next(self, within)
    }
}

/// Where a bridge's streams come from.
trait Server {
    fn open<'w>(&'w self, stop: &'w Wake) -> Result<Box<dyn Stream + 'w>, String>;
}

struct Socket {
    path: PathBuf,
    state: PathBuf,
}

impl Server for Socket {
    fn open<'w>(&'w self, stop: &'w Wake) -> Result<Box<dyn Stream + 'w>, String> {
        Ok(Box::new(RexEvents::open(
            &self.path,
            &self.state,
            stop,
            Liveness::REX,
        )?))
    }
}

/// How long the bridge waits before each new try at its server, and when it
/// gives up.
struct Away {
    delays: &'static [Duration],
    give_up: Duration,
}

impl Away {
    const REX: Self = Self {
        delays: &[
            Duration::from_millis(100),
            Duration::from_millis(250),
            Duration::from_millis(500),
            Duration::from_secs(1),
            Duration::from_secs(5),
        ],
        give_up: Duration::from_secs(120),
    };

    fn delay(&self, tries: usize) -> Duration {
        self.delays[tries.min(self.delays.len() - 1)]
    }
}

/// How long one wait for an event lasts while nothing is pending, which is
/// also how long a bridge outlives its state directory. The stream pings on
/// its own schedule within it.
const IDLE: Duration = Duration::from_secs(60);

/// The bridge's loop. Each connection queues a `Resync` for every activated
/// space of the endpoint, unless the server's `host_id` changed, then
/// coalesces events into `Pending` and flushes them when they are due. A
/// stop flushes what is pending and returns. So does a server that has
/// stayed away for `away.give_up`, or one never reached at all. A lost lock
/// returns at once, because the state it would write to is gone.
fn bridge(
    state: &Path,
    endpoint: &Endpoint,
    server: &dyn Server,
    stop: &Wake,
    lock: &Lock,
    spawn: &mut dyn FnMut(&SpaceId) -> Result<Child, String>,
    away: &Away,
) {
    let mut reached: Option<Instant> = None;
    let mut tries = 0;
    while !stop.is_set() && lock.held() {
        let mut stream = match server.open(stop) {
            Ok(stream) => stream,
            Err(_) => {
                if reached.is_none_or(|at| at.elapsed() >= away.give_up)
                    || stop.sleep(away.delay(tries))
                {
                    return;
                }
                tries += 1;
                continue;
            }
        };
        tries = 0;
        let mut pending = Pending::default();
        if same_server(state, endpoint, stream.host_id()) {
            for space in activated_spaces(state, endpoint) {
                pending.note(MuxEvent::Resync(space), Instant::now());
            }
        }
        loop {
            if stop.is_set() {
                flush(state, endpoint, &mut pending, spawn);
                return;
            }
            if !lock.held() {
                return;
            }
            let now = Instant::now();
            if pending.due().is_some_and(|due| due <= now) {
                flush(state, endpoint, &mut pending, spawn);
            }
            let within = pending
                .due()
                .map_or(IDLE, |due| due.saturating_duration_since(now));
            match stream.next(within) {
                Next::Event(event) => pending.note(event, Instant::now()),
                Next::Quiet => {}
                Next::Ended(_) => {
                    flush(state, endpoint, &mut pending, spawn);
                    break;
                }
            }
        }
        reached = Some(Instant::now());
    }
}

/// Queues the pending events of each activated space under the command
/// gate, after a replaced runtime has taken over, and starts each space's
/// worker once. The bridge outlives its workers, so it reaps each one as it
/// exits.
fn flush(
    state: &Path,
    endpoint: &Endpoint,
    pending: &mut Pending,
    spawn: &mut dyn FnMut(&SpaceId) -> Result<Child, String>,
) {
    let batch = pending.take();
    if batch.is_empty() {
        return;
    }
    let Ok(_gate) = crate::runtime_command_guard(state, None) else {
        return;
    };
    let _ = crate::runtime_reexec_if_replaced(state);
    let activated = activated_spaces(state, endpoint);
    for (space, events) in batch {
        if !activated.contains(&space) {
            continue;
        }
        let queued = events
            .into_iter()
            .filter(|event| {
                status_queue::enqueue(state, endpoint, &space, &Marker::Event(event.clone()))
                    .is_ok()
            })
            .count();
        if queued > 0
            && let Ok(mut worker) = spawn(&space)
        {
            std::thread::spawn(move || worker.wait());
        }
    }
}

fn activated_spaces(state: &Path, endpoint: &Endpoint) -> Vec<SpaceId> {
    StateStore::open_read_only(state)
        .ok()
        .flatten()
        .and_then(|mut store| {
            store
                .transaction(|transaction| transaction.activated_spaces(&endpoint.key))
                .ok()
        })
        .unwrap_or_default()
}

#[derive(Deserialize, Serialize)]
struct KnownServer {
    host_id: String,
}

/// Whether the server behind `endpoint` is the one the bridge knew. The
/// first `host_id` seen is stored and never replaced. Another server at the
/// same socket has other state, so its bridge retires nothing until a person
/// runs `namedrop reconcile` there.
fn same_server(state: &Path, endpoint: &Endpoint, host_id: Option<&str>) -> bool {
    let path = state
        .join("rex/servers")
        .join(format!("{}.json", scope(endpoint)));
    let known = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<KnownServer>(&bytes).ok());
    match (known, host_id) {
        (Some(known), Some(host_id)) => known.host_id == host_id,
        (None, Some(host_id)) => {
            let directory = path.parent().expect("a server file has a directory");
            let _ = fs::create_dir_all(directory).and_then(|()| {
                fs::write(
                    &path,
                    serde_json::to_vec(&KnownServer {
                        host_id: host_id.to_owned(),
                    })?,
                )
            });
            true
        }
        (_, None) => true,
    }
}

#[cfg(test)]
mod tests;
