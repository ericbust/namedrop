use super::ws::{Message, WebSocket, WsError};
use crate::mux::Budget;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    os::unix::net::UnixStream,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

const PROTOCOL: &str = "rex.control.v1";
/// The only control API revision this client has been checked against.
const API_REVISION: u64 = 12;
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_TIMEOUT: Duration = Duration::from_secs(60);
pub(super) const MAX_CALL_BYTES: usize = 4 * 1024 * 1024;
const CANCEL_POLL: Duration = Duration::from_millis(10);

pub(super) trait Timed: Read + Write {
    /// How long the next reads wait. Zero makes them return at once.
    fn read_within(&self, timeout: Duration);
    fn write_within(&self, timeout: Duration);
}

/// macOS refuses a socket option once the peer has hung up, though the bytes
/// it sent are still readable. The timeout set before then stays, and the
/// next read or write reports the hang-up itself, so the refusal is ignored.
impl Timed for UnixStream {
    fn read_within(&self, timeout: Duration) {
        let _ = self.set_nonblocking(timeout.is_zero());
        if !timeout.is_zero() {
            let _ = self.set_read_timeout(Some(timeout));
        }
    }

    fn write_within(&self, timeout: Duration) {
        let _ = self.set_nonblocking(false);
        let _ = self.set_write_timeout(Some(timeout));
    }
}

pub(super) struct Limits<'a> {
    pub budget: Box<dyn Fn() -> Result<Budget, String> + 'a>,
    pub cancelled: &'a AtomicBool,
}

/// Opens one control connection: upgrade, hello, welcome. Scripted in tests.
pub(super) trait Wire: Send + Sync {
    /// The connection and the pid of the process at its other end. Each
    /// request is held to `narrower` as well as to the wire's own budget.
    fn open<'a>(&'a self, narrower: Narrower<'a>) -> Result<(Box<dyn Control + 'a>, u32), Fault>;
    /// The timeout of the budget now, for a caller that must know before it
    /// starts whether a whole call fits.
    fn timeout(&self) -> Result<Duration, String>;
}

/// What a `within` adds to a wire's limits. Its budget is asked for before
/// each request, and its flag, when it has one, cancels in place of the
/// wire's own.
pub(super) struct Narrower<'a> {
    pub budget: Box<dyn Fn() -> Result<Budget, String> + 'a>,
    pub cancelled: Option<&'a AtomicBool>,
}

impl Narrower<'_> {
    pub(super) fn none() -> Self {
        Self {
            budget: Box::new(|| Ok(Budget::ADAPTER_DEFAULT)),
            cancelled: None,
        }
    }
}

impl<W: Wire + ?Sized> Wire for &W {
    fn open<'a>(&'a self, narrower: Narrower<'a>) -> Result<(Box<dyn Control + 'a>, u32), Fault> {
        (**self).open(narrower)
    }

    fn timeout(&self) -> Result<Duration, String> {
        (**self).timeout()
    }
}

pub(super) trait Control {
    /// One request and its response.
    fn request(&mut self, method: &str, payload: Option<Value>) -> Result<Reply, Fault>;
    /// A buffered event, or the next to arrive within `within`. A zero wait
    /// still reads what the socket already holds.
    fn next_event(&mut self, within: Duration) -> Result<Option<Event>, Fault>;
}

impl<S: Timed> Control for Connection<'_, S> {
    fn request(&mut self, method: &str, payload: Option<Value>) -> Result<Reply, Fault> {
        let replied = self.try_request(method, payload);
        self.settle(replied)
    }

    /// `Duration::MAX` waits until an event arrives or the flag is set.
    /// Events buffered before the connection broke are still returned.
    fn next_event(&mut self, within: Duration) -> Result<Option<Event>, Fault> {
        let next = self.try_next_event(within);
        self.settle(next)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Fault {
    Rex {
        code: String,
        message: String,
        status: u16,
    },
    Timeout(Duration),
    TooLarge,
    Closed(String),
    Cancelled,
    Allowance(String),
    /// The server answered, and it is not one on this machine.
    NotLocal(String),
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Reply {
    /// The last event queued for this connection before the server started
    /// the request. The result already includes every event up to it.
    pub seq: u64,
    pub payload: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Event {
    pub seq: u64,
    pub payload: Value,
}

pub(super) struct Connection<'a, S: Timed> {
    socket: WebSocket<S>,
    limits: Limits<'a>,
    timeout_limit: Duration,
    events: VecDeque<Event>,
    next_id: u64,
    max_request: usize,
    /// Set by any fault but a Rex error, after which the stream may hold half
    /// a frame in either direction, so no later request or read runs on it.
    broken: bool,
}

impl<'a, S: Timed> Connection<'a, S> {
    /// The upgrade, hello, and welcome together take one request's budget
    /// and stop when the flag is set.
    pub(super) fn open(stream: S, limits: Limits<'a>) -> Result<Self, Fault> {
        let timeout_limit = request_timeout_cap();
        let budget = request_budget(&limits, timeout_limit)?;
        if budget.timeout.is_zero() {
            return Err(Fault::Timeout(budget.timeout));
        }
        let deadline = Instant::now() + budget.timeout;
        let mut socket = WebSocket::connect(stream, "/api/control", PROTOCOL, MAX_CALL_BYTES)
            .map_err(|error| fault(error, budget.timeout))?;
        socket.set_allowance(MAX_CALL_BYTES);
        let mut connection = Self {
            socket,
            limits,
            timeout_limit,
            events: VecDeque::new(),
            next_id: 0,
            max_request: usize::MAX,
            broken: false,
        };
        let welcome = connection.handshake(budget, deadline);
        let welcome = connection.settle(welcome)?;
        let limit = welcome["limits"]["max_message_bytes"].as_u64();
        match (
            welcome["type"].as_str(),
            welcome["protocol"].as_str(),
            welcome["api_revision"].as_u64(),
            limit,
        ) {
            (Some("welcome"), Some(PROTOCOL), Some(API_REVISION), Some(limit)) => {
                connection.max_request = usize::try_from(limit).unwrap_or(usize::MAX);
                Ok(connection)
            }
            _ => Err(Fault::Closed(format!(
                "the server's welcome is not one for {PROTOCOL} revision {API_REVISION}: {welcome}"
            ))),
        }
    }

    /// The upgrade request and its answer, the hello, and the welcome.
    fn handshake(&mut self, budget: Budget, deadline: Instant) -> Result<Value, Fault> {
        self.poll(Some(deadline), budget.timeout, Way::Write, |socket| {
            socket.flush().map(|done| done.then_some(()))
        })?
        .ok_or(Fault::Timeout(budget.timeout))?;
        self.poll(Some(deadline), budget.timeout, Way::Read, |socket| {
            socket.try_upgrade().map(|done| done.then_some(()))
        })?
        .ok_or(Fault::Timeout(budget.timeout))?;
        let hello = json!({
            "type": "hello",
            "api_revision": API_REVISION,
            "api_min_revision": API_REVISION,
            "client": {"kind": "other", "name": "namedrop", "version": env!("CARGO_PKG_VERSION")},
        });
        self.exchange(budget, deadline, &hello, |_| true)
    }

    fn try_request(&mut self, method: &str, payload: Option<Value>) -> Result<Reply, Fault> {
        let budget = request_budget(&self.limits, self.timeout_limit)?;
        self.intact()?;
        let deadline = Instant::now() + budget.timeout;
        self.next_id += 1;
        let id = self.next_id.to_string();
        let mut request = json!({"type": "request", "id": id, "method": method});
        if let Some(payload) = payload {
            request["payload"] = payload;
        }
        let response = self.exchange(budget, deadline, &request, |message| {
            message["type"] == "response" && message["id"] == *id
        })?;
        if let Some(error) = response.get("error").filter(|error| !error.is_null()) {
            return Err(Fault::Rex {
                code: error["code"].as_str().unwrap_or_default().to_owned(),
                message: error["message"].as_str().unwrap_or_default().to_owned(),
                status: error["status"]
                    .as_u64()
                    .and_then(|status| u16::try_from(status).ok())
                    .unwrap_or(500),
            });
        }
        let seq = response["seq"].as_u64().ok_or_else(|| {
            Fault::Closed(format!(
                "the server sent a response with no seq: {response}"
            ))
        })?;
        Ok(Reply {
            seq,
            payload: response.get("payload").cloned().unwrap_or(Value::Null),
        })
    }

    fn try_next_event(&mut self, within: Duration) -> Result<Option<Event>, Fault> {
        if let Some(event) = self.events.pop_front() {
            return Ok(Some(event));
        }
        self.intact()?;
        let deadline = Instant::now().checked_add(within);
        while let Some(message) = self.receive(deadline, within)? {
            if message["type"] == "event" {
                return parse_event(&message).map(Some);
            }
        }
        Ok(None)
    }

    fn intact(&self) -> Result<(), Fault> {
        if self.broken {
            return Err(Fault::Closed(
                "the connection broke on an earlier request".into(),
            ));
        }
        Ok(())
    }

    fn settle<T>(&mut self, result: Result<T, Fault>) -> Result<T, Fault> {
        if matches!(&result, Err(fault) if !matches!(fault, Fault::Rex { .. })) {
            self.broken = true;
        }
        result
    }

    fn exchange(
        &mut self,
        budget: Budget,
        deadline: Instant,
        message: &Value,
        answers: impl Fn(&Value) -> bool,
    ) -> Result<Value, Fault> {
        let text = message.to_string();
        if text.len() > self.max_request {
            return Err(Fault::TooLarge);
        }
        let left = self.socket.allowance();
        let held = left.min(budget.max_output_bytes);
        self.socket.set_allowance(held);
        let answered = self.send_and_wait(deadline, budget.timeout, &text, answers);
        let used = held - self.socket.allowance();
        self.socket.set_allowance(left - used);
        answered
    }

    fn send_and_wait(
        &mut self,
        deadline: Instant,
        timeout: Duration,
        text: &str,
        answers: impl Fn(&Value) -> bool,
    ) -> Result<Value, Fault> {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Fault::Timeout(timeout));
        }
        self.socket
            .queue_text(text)
            .map_err(|error| fault(error, timeout))?;
        self.poll(Some(deadline), timeout, Way::Write, |socket| {
            socket.flush().map(|done| done.then_some(()))
        })?
        .ok_or(Fault::Timeout(timeout))?;
        loop {
            let Some(message) = self.receive(Some(deadline), timeout)? else {
                return Err(Fault::Timeout(timeout));
            };
            if answers(&message) {
                return Ok(message);
            }
            if message["type"] == "event" {
                let event = parse_event(&message)?;
                self.events.push_back(event);
            }
        }
    }

    fn receive(
        &mut self,
        deadline: Option<Instant>,
        limit: Duration,
    ) -> Result<Option<Value>, Fault> {
        match self.poll(deadline, limit, Way::Read, WebSocket::try_recv)? {
            None => Ok(None),
            Some(Message::Text(text)) => serde_json::from_str(&text)
                .map(Some)
                .map_err(|_| Fault::Closed("the server sent a message that is not JSON".into())),
            Some(Message::Binary(_)) => {
                Err(Fault::Closed("the server sent a binary message".into()))
            }
        }
    }

    /// Runs `step`, each run one read or one write of at most `CANCEL_POLL`,
    /// until it finishes, the deadline passes, or the flag is set. A step
    /// that made progress and is not done answers `None`. A deadline already
    /// passed still gets one run that does not wait, and no deadline waits
    /// for the flag.
    fn poll<T>(
        &mut self,
        deadline: Option<Instant>,
        limit: Duration,
        way: Way,
        mut step: impl FnMut(&mut WebSocket<S>) -> Result<Option<T>, WsError>,
    ) -> Result<Option<T>, Fault> {
        let mut ran = false;
        loop {
            if self.limits.cancelled.load(Ordering::Relaxed) {
                return Err(Fault::Cancelled);
            }
            let left = deadline.map_or(CANCEL_POLL, |deadline| {
                deadline.saturating_duration_since(Instant::now())
            });
            if left.is_zero() && ran {
                return Ok(None);
            }
            ran = true;
            let stream = self.socket.stream();
            match way {
                Way::Read => stream.read_within(left.min(CANCEL_POLL)),
                Way::Write => stream.write_within(left.min(CANCEL_POLL)),
            }
            match step(&mut self.socket) {
                Ok(Some(done)) => return Ok(Some(done)),
                Ok(None) => {}
                Err(WsError::Io(error))
                    if timed_out(&error) || error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(fault(error, limit)),
            }
        }
    }
}

/// The bridge's long wait for events, which sleeps in `poll(2)` until the
/// socket or a wake pipe is readable, rather than in short read steps.
impl<S: Timed + std::os::fd::AsFd> Connection<'_, S> {
    /// The next event, or `None` once `deadline` passes. A readable `wake`
    /// cancels the wait. Each message may use the call's whole allowance,
    /// because a stream that lasts for days reads far more than one call.
    pub(super) fn wait_event(
        &mut self,
        deadline: Option<Instant>,
        wake: std::os::fd::BorrowedFd<'_>,
    ) -> Result<Option<Event>, Fault> {
        let waited = self.try_wait_event(deadline, wake);
        self.settle(waited)
    }

    fn try_wait_event(
        &mut self,
        deadline: Option<Instant>,
        wake: std::os::fd::BorrowedFd<'_>,
    ) -> Result<Option<Event>, Fault> {
        // Bytes read earlier may hold a whole frame, which `poll(2)` cannot
        // see. Once a read takes no message, what is left is part of one.
        let mut whole = self.socket.buffered();
        let mut ran = false;
        loop {
            if let Some(event) = self.events.pop_front() {
                return Ok(Some(event));
            }
            self.intact()?;
            if self.limits.cancelled.load(Ordering::Relaxed) {
                return Err(Fault::Cancelled);
            }
            let left = deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()));
            if ran && left.is_some_and(|left| left.is_zero()) {
                return Ok(None);
            }
            ran = true;
            if !whole {
                match readable(self.socket.stream().as_fd(), wake, left)? {
                    Readable::Socket => {}
                    Readable::Wake => return Err(Fault::Cancelled),
                    Readable::Neither => return Ok(None),
                }
            }
            self.socket.set_allowance(MAX_CALL_BYTES);
            self.socket.stream().read_within(CANCEL_POLL);
            let received = self.socket.try_recv();
            whole = matches!(received, Ok(Some(_))) && self.socket.buffered();
            match received {
                Ok(Some(Message::Text(text))) => {
                    let message: Value = serde_json::from_str(&text).map_err(|_| {
                        Fault::Closed("the server sent a message that is not JSON".into())
                    })?;
                    if message["type"] == "event" {
                        return parse_event(&message).map(Some);
                    }
                }
                Ok(Some(Message::Binary(_))) => {
                    return Err(Fault::Closed("the server sent a binary message".into()));
                }
                Ok(None) => {}
                Err(WsError::Io(error))
                    if timed_out(&error) || error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(fault(error, CANCEL_POLL)),
            }
        }
    }

    /// How many frames the server has sent on this connection.
    pub(super) fn frames(&self) -> u64 {
        self.socket.frames()
    }

    /// Sends a ping within one request's budget.
    pub(super) fn ping(&mut self) -> Result<(), Fault> {
        let pinged = self.try_ping();
        self.settle(pinged)
    }

    fn try_ping(&mut self) -> Result<(), Fault> {
        let budget = request_budget(&self.limits, self.timeout_limit)?;
        self.intact()?;
        self.socket
            .queue_ping()
            .map_err(|error| fault(error, budget.timeout))?;
        self.poll(
            Some(Instant::now() + budget.timeout),
            budget.timeout,
            Way::Write,
            |socket| socket.flush().map(|done| done.then_some(())),
        )?
        .ok_or(Fault::Timeout(budget.timeout))
    }
}

enum Readable {
    Socket,
    Wake,
    Neither,
}

/// One `poll(2)` of `socket` and `wake`, for `within` or with no end. The
/// wake wins when both are readable.
fn readable(
    socket: std::os::fd::BorrowedFd<'_>,
    wake: std::os::fd::BorrowedFd<'_>,
    within: Option<Duration>,
) -> Result<Readable, Fault> {
    use std::os::fd::AsRawFd;
    let timeout = within.map_or(-1, |within| {
        libc::c_int::try_from(
            within
                .as_millis()
                .saturating_add(u128::from(within.subsec_nanos() % 1_000_000 != 0)),
        )
        .unwrap_or(libc::c_int::MAX)
    });
    let mut fds = [
        libc::pollfd {
            fd: socket.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: wake.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    // SAFETY: `fds` is a live array of two pollfd, and both descriptors are
    // borrowed for the call.
    let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout) };
    if ready < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return Ok(Readable::Neither);
        }
        return Err(Fault::Closed(format!("could not wait for Rex: {error}")));
    }
    Ok(if fds[1].revents != 0 {
        Readable::Wake
    } else if fds[0].revents != 0 {
        Readable::Socket
    } else {
        Readable::Neither
    })
}

#[derive(Clone, Copy)]
enum Way {
    Read,
    Write,
}

/// A write cut short leaves half a frame on the wire, so a close frame after
/// it would be garbage. Otherwise the close is written as each step writes,
/// within `CANCEL_POLL`, the last write timeout `poll` set.
impl<S: Timed> Drop for Connection<'_, S> {
    fn drop(&mut self) {
        if self.socket.unsent() {
            return;
        }
        let _ = self.socket.close(1000);
    }
}

fn request_budget(limits: &Limits<'_>, cap: Duration) -> Result<Budget, Fault> {
    if limits.cancelled.load(Ordering::Relaxed) {
        return Err(Fault::Cancelled);
    }
    let budget = (limits.budget)().map_err(Fault::Allowance)?;
    Ok(Budget {
        timeout: budget.timeout.min(cap),
        max_output_bytes: budget.max_output_bytes,
    })
}

/// The most any one request may take: `NAMEDROP_REX_TIMEOUT_MS`, or 5 s.
fn request_timeout_cap() -> Duration {
    timeout_cap(std::env::var("NAMEDROP_REX_TIMEOUT_MS").ok().as_deref())
}

fn timeout_cap(value: Option<&str>) -> Duration {
    explicit_cap(value).unwrap_or(DEFAULT_TIMEOUT)
}

/// `NAMEDROP_REX_TIMEOUT_MS` when it is set to a usable value, without the
/// 5 s default.
pub(super) fn explicit_timeout_cap() -> Option<Duration> {
    explicit_cap(std::env::var("NAMEDROP_REX_TIMEOUT_MS").ok().as_deref())
}

fn explicit_cap(value: Option<&str>) -> Option<Duration> {
    value
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|milliseconds| *milliseconds > 0)
        .map(|milliseconds| Duration::from_millis(milliseconds).min(MAX_TIMEOUT))
}

fn parse_event(message: &Value) -> Result<Event, Fault> {
    let seq = message["seq"]
        .as_u64()
        .ok_or_else(|| Fault::Closed(format!("the server sent an event with no seq: {message}")))?;
    Ok(Event {
        seq,
        payload: message["payload"].clone(),
    })
}

fn timed_out(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}

fn fault(error: WsError, limit: Duration) -> Fault {
    match error {
        WsError::Io(error) if timed_out(&error) => Fault::Timeout(limit),
        WsError::Io(error) => Fault::Closed(format!("the connection failed: {error}")),
        WsError::Closed { code: None, .. } => Fault::Closed("the connection ended".into()),
        WsError::Closed {
            code: Some(code),
            reason,
        } => Fault::Closed(format!(
            "the server closed the connection ({code} {reason})"
        )),
        WsError::TooLarge => Fault::TooLarge,
        WsError::Protocol(error) => {
            Fault::Closed(format!("the server broke the protocol: {error}"))
        }
    }
}

#[cfg(test)]
pub(super) mod tests;
