//! The Rex bridge's stream: one control connection attached to every session
//! of a server, whose events decode into the lifecycle events the status
//! worker handles, and the coalescing that bounds what the bridge queues.

use super::{
    binding::Bindings,
    control::Control,
    control::{Connection, Fault, Limits},
    local,
};
use crate::{
    SpaceId,
    mux::{Budget, MuxEvent, PaneId, PaneSlot, TabId},
};
use serde_json::{Value, json};
use std::{
    io,
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd},
        unix::net::UnixStream,
    },
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

/// A stop request that a signal handler can make: a flag, and a pipe whose
/// byte wakes a stream waiting in `poll(2)`.
pub(crate) struct Wake {
    set: AtomicBool,
    read: OwnedFd,
    write: OwnedFd,
}

impl Wake {
    pub(crate) fn new() -> io::Result<Self> {
        let mut fds = [0; 2];
        // SAFETY: `pipe` writes two descriptors into the array it is given.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `pipe` just opened both, and nothing else owns them.
        let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        for fd in [&read, &write] {
            // SAFETY: `fcntl` changes only the flags of a descriptor this owns.
            unsafe {
                libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
                let flags = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
                libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }
        Ok(Self {
            set: AtomicBool::new(false),
            read,
            write,
        })
    }

    /// Async-signal-safe: one store and one `write(2)`.
    pub(crate) fn wake(&self) {
        self.set.store(true, Ordering::SeqCst);
        // SAFETY: writes one byte from a live buffer to a descriptor this
        // owns. A full pipe already wakes its reader.
        unsafe { libc::write(self.write.as_raw_fd(), [1_u8].as_ptr().cast(), 1) };
    }

    pub(crate) fn is_set(&self) -> bool {
        self.set.load(Ordering::SeqCst)
    }

    /// Waits `within`, or less once a stop is requested, and says whether
    /// one was.
    pub(crate) fn sleep(&self, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        while !self.is_set() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let mut fd = libc::pollfd {
                fd: self.read.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let timeout =
                libc::c_int::try_from(left.as_millis().max(1)).unwrap_or(libc::c_int::MAX);
            // SAFETY: one live pollfd for a descriptor this owns.
            unsafe { libc::poll(&mut fd, 1, timeout) };
        }
        self.is_set()
    }

    pub(super) fn fd(&self) -> BorrowedFd<'_> {
        self.read.as_fd()
    }
}

/// How long a stream may stay silent before the bridge pings, and how long
/// it then waits for anything at all from the server.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Liveness {
    pub quiet: Duration,
    pub answer: Duration,
}

impl Liveness {
    /// The server pings every 120 s and waits 60 s for each answer, so the
    /// bridge does the same.
    pub(crate) const REX: Self = Self {
        quiet: Duration::from_secs(120),
        answer: Duration::from_secs(60),
    };
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Next {
    Event(MuxEvent),
    /// The wait passed with no event the bridge handles.
    Quiet,
    /// The connection is over, and why. In-flight mail is untouched.
    Ended(String),
}

pub(crate) struct RexEvents<'a> {
    connection: Connection<'a, UnixStream>,
    wake: &'a Wake,
    bindings: Bindings,
    host_id: Option<String>,
    liveness: Liveness,
    /// How many frames the server had sent when the stream last heard it,
    /// and when.
    heard: (u64, Instant),
    pinged: Option<Instant>,
}

impl<'a> RexEvents<'a> {
    /// Dials `socket`, proves the server local, and attaches to every
    /// session's events. A block's binding lives in `state`, and `wake`
    /// cancels every request and wait.
    pub(crate) fn open(
        socket: &Path,
        state: &Path,
        wake: &'a Wake,
        liveness: Liveness,
    ) -> Result<Self, String> {
        let stream = UnixStream::connect(socket)
            .map_err(|error| format!("could not reach Rex at {}: {error}", socket.display()))?;
        Self::over(stream, socket, state, wake, liveness)
    }

    /// `open` over a stream already connected to `socket`.
    fn over(
        stream: UnixStream,
        socket: &Path,
        state: &Path,
        wake: &'a Wake,
        liveness: Liveness,
    ) -> Result<Self, String> {
        let peer = local::peer_pid(&stream).map_err(|error| {
            format!(
                "{} cannot be proved a Rex server on this machine: {error}",
                socket.display()
            )
        })?;
        let mut connection = Connection::open(
            stream,
            Limits {
                budget: Box::new(|| Ok(Budget::ADAPTER_DEFAULT)),
                cancelled: &wake.set,
            },
        )
        .map_err(said)?;
        let server = local::prove_local(socket, &mut connection, peer).map_err(said)?;
        connection
            .request("session.attach_all", Some(json!({"event_scope": "all"})))
            .map_err(said)?;
        Ok(Self {
            heard: (connection.frames(), Instant::now()),
            connection,
            wake,
            bindings: Bindings::new(state),
            host_id: server.host_id,
            liveness,
            pinged: None,
        })
    }

    /// The server's `host_id`, as its status reported it.
    pub(crate) fn host_id(&self) -> Option<&str> {
        self.host_id.as_deref()
    }

    /// The next event that decodes into a lifecycle event, waiting at most
    /// `within`. A server silent for `liveness.quiet` gets a ping, and one
    /// that then sends nothing for `liveness.answer` ends the stream.
    pub(crate) fn next(&mut self, within: Duration) -> Next {
        let until = Instant::now().checked_add(within);
        loop {
            let now = Instant::now();
            let frames = self.connection.frames();
            if frames != self.heard.0 {
                self.heard = (frames, now);
                self.pinged = None;
            }
            let alive_until = match self.pinged {
                None => {
                    let quiet_until = self.heard.1 + self.liveness.quiet;
                    if now >= quiet_until {
                        if let Err(fault) = self.connection.ping() {
                            return Next::Ended(said(fault));
                        }
                        self.pinged = Some(now);
                        continue;
                    }
                    quiet_until
                }
                Some(at) if now >= at + self.liveness.answer => {
                    return Next::Ended(format!(
                        "the server sent nothing for {} s after a ping",
                        self.liveness.answer.as_secs_f64()
                    ));
                }
                Some(at) => at + self.liveness.answer,
            };
            let deadline = until.map_or(alive_until, |until| until.min(alive_until));
            match self.connection.wait_event(Some(deadline), self.wake.fd()) {
                Ok(Some(event)) => {
                    if let Some(event) = self.decode(&event.payload) {
                        return Next::Event(event);
                    }
                }
                Ok(None) => {}
                Err(fault) => return Next::Ended(said(fault)),
            }
            if until.is_some_and(|until| Instant::now() >= until) {
                return Next::Quiet;
            }
        }
    }

    /// The decode table. A closed block's binding is deleted before its
    /// `PaneClosed` is handed on, so no later bind can be read for it.
    fn decode(&self, payload: &Value) -> Option<MuxEvent> {
        let fields = &payload["payload"];
        match payload["type"].as_str()? {
            "session_destroyed" => Some(MuxEvent::SpaceClosed(SpaceId::new(
                fields["session_id"].as_str()?,
            ))),
            "session_event" => {
                let space = SpaceId::new(fields["session_id"].as_str()?);
                let event = &fields["event"];
                let block = || event["payload"]["block_id"].as_str();
                let slot = |block: &str| PaneSlot {
                    space: space.clone(),
                    pane: PaneId::new(block),
                    tab: None,
                };
                match event["type"].as_str()? {
                    "block_created" => Some(MuxEvent::PaneAppeared(slot(block()?))),
                    "block_closed" => {
                        let block = block()?;
                        let _ = self.bindings.forget(block);
                        Some(MuxEvent::PaneClosed(slot(block)))
                    }
                    "window_closed" => Some(MuxEvent::TabClosed {
                        tab: TabId::new(event["payload"]["window_id"].as_str()?),
                        space,
                    }),
                    "block_event" if status_change(&event["payload"]) => {
                        Some(MuxEvent::AgentStatusChanged(slot(block()?)))
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }
}

/// A `block_event` that can change what the block's agent is or reports:
/// its root status record set or cleared, its foreground process changed,
/// or its program exited.
fn status_change(block_event: &Value) -> bool {
    let fields = &block_event["payload"];
    match block_event["name"].as_str() {
        Some("program_status_changed") => fields["id"] == "",
        Some("program_status_removed") => fields["ids"]
            .as_array()
            .is_some_and(|ids| ids.iter().any(|id| id == "")),
        Some("process_changed" | "child_exited") => true,
        _ => false,
    }
}

fn said(fault: Fault) -> String {
    match fault {
        Fault::Rex { code, message, .. } => format!("{message} ({code})"),
        Fault::Timeout(limit) => format!("Rex request timed out after {}ms", limit.as_millis()),
        Fault::TooLarge => "Rex sent a message over its limit".to_owned(),
        Fault::Cancelled => "stopped".to_owned(),
        Fault::Closed(text) | Fault::Allowance(text) | Fault::NotLocal(text) => text,
    }
}

/// The events the bridge has not queued yet, per space in arrival order.
/// Each space holds at most one status change per pane, a `Resync` absorbs
/// its space's status changes, and a `SpaceClosed` absorbs everything else
/// for its space, so what waits is bounded by the server's blocks.
#[derive(Debug, Default)]
pub(crate) struct Pending {
    spaces: Vec<(SpaceId, Vec<MuxEvent>)>,
    first: Option<Instant>,
    last: Option<Instant>,
}

/// A flush follows the last event by this much, as long as no event comes.
const SETTLE: Duration = Duration::from_millis(50);
/// No pending change waits longer than this for its flush.
const LONGEST: Duration = Duration::from_millis(250);

impl Pending {
    pub(crate) fn note(&mut self, event: MuxEvent, now: Instant) {
        self.first.get_or_insert(now);
        self.last = Some(now);
        let space = event.space().clone();
        let queue = match self.spaces.iter_mut().find(|(known, _)| *known == space) {
            Some((_, queue)) => queue,
            None => {
                self.spaces.push((space, Vec::new()));
                &mut self.spaces.last_mut().expect("just pushed").1
            }
        };
        if queue
            .iter()
            .any(|queued| matches!(queued, MuxEvent::SpaceClosed(_)))
        {
            return;
        }
        match &event {
            MuxEvent::SpaceClosed(_) => queue.clear(),
            MuxEvent::Resync(_) => {
                queue.retain(|queued| !matches!(queued, MuxEvent::AgentStatusChanged(_)));
                if queue.contains(&event) {
                    return;
                }
            }
            MuxEvent::AgentStatusChanged(slot) => {
                let absorbed = queue.iter().any(|queued| match queued {
                    MuxEvent::Resync(_) => true,
                    MuxEvent::AgentStatusChanged(queued) => queued.pane == slot.pane,
                    _ => false,
                });
                if absorbed {
                    return;
                }
            }
            _ => {}
        }
        queue.push(event);
    }

    /// When the pending events should be queued, or `None` with none.
    pub(crate) fn due(&self) -> Option<Instant> {
        Some((self.last? + SETTLE).min(self.first? + LONGEST))
    }

    pub(crate) fn take(&mut self) -> Vec<(SpaceId, Vec<MuxEvent>)> {
        self.first = None;
        self.last = None;
        std::mem::take(&mut self.spaces)
    }
}

#[cfg(test)]
mod tests;

/// A scripted server for tests outside this module.
#[cfg(test)]
pub(crate) mod tests_support {
    use super::{Liveness, RexEvents, Wake};
    use crate::mux::rex::control::tests::Server;
    use serde_json::{Value, json};
    use std::{os::unix::net::UnixStream, path::Path};

    /// A local server with `host_id` that sends `events` as soon as the
    /// stream attaches, then hangs up.
    pub(crate) struct Scripted(UnixStream);

    impl Scripted {
        pub(crate) fn new(host_id: &'static str, events: Vec<Value>) -> Self {
            let (client, server) = UnixStream::pair().unwrap();
            std::thread::spawn(move || {
                let mut server = Server::upgrade(server);
                server.welcome();
                for payload in [
                    json!({"pid": std::process::id(), "host_id": host_id, "listeners": [
                        {"primary": true, "transport": "unix", "addresses": ["/tmp/r.sock"]}]}),
                    json!({"sessions": []}),
                ] {
                    let request = server.message();
                    server.send(json!({"type": "response", "id": request["id"], "seq": 0, "payload": payload}));
                }
                for (seq, payload) in events.into_iter().enumerate() {
                    server.send(json!({"type": "event", "seq": seq + 1, "payload": payload}));
                }
            });
            Self(client)
        }

        pub(crate) fn events<'w>(self, state: &Path, wake: &'w Wake) -> RexEvents<'w> {
            RexEvents::over(self.0, Path::new("/tmp/r.sock"), state, wake, Liveness::REX).unwrap()
        }
    }
}
