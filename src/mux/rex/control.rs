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
    pub budget: &'a (dyn Fn() -> Result<Budget, String> + 'a),
    pub cancelled: &'a AtomicBool,
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
        let timeout_limit = timeout_cap(std::env::var("NAMEDROP_REX_TIMEOUT_MS").ok().as_deref());
        let budget = request_budget(&limits, timeout_limit)?;
        if budget.timeout.is_zero() {
            return Err(Fault::Timeout(budget.timeout));
        }
        let deadline = Instant::now() + budget.timeout;
        stream.write_within(budget.timeout);
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
        connection
            .poll(Some(deadline), budget.timeout, WebSocket::upgrade)?
            .ok_or(Fault::Timeout(budget.timeout))?;
        let hello = json!({
            "type": "hello",
            "api_revision": API_REVISION,
            "api_min_revision": API_REVISION,
            "client": {"kind": "other", "name": "namedrop", "version": env!("CARGO_PKG_VERSION")},
        });
        let welcome = connection.exchange(budget, deadline, &hello, |_| true)?;
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

    pub(super) fn request(&mut self, method: &str, payload: Option<Value>) -> Result<Reply, Fault> {
        let replied = self.try_request(method, payload);
        self.settle(replied)
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

    /// A buffered event, or the next to arrive within `within`. A zero wait
    /// still reads what the socket already holds, and `Duration::MAX` waits
    /// until an event arrives or the flag is set. Events buffered before the
    /// connection broke are still returned.
    pub(super) fn next_event(&mut self, within: Duration) -> Result<Option<Event>, Fault> {
        let next = self.try_next_event(within);
        self.settle(next)
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
        self.socket.stream().write_within(left);
        self.socket
            .send_text(text)
            .map_err(|error| fault(error, timeout))?;
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
        match self.poll(deadline, limit, WebSocket::recv)? {
            None => Ok(None),
            Some(Message::Text(text)) => serde_json::from_str(&text)
                .map(Some)
                .map_err(|_| Fault::Closed("the server sent a message that is not JSON".into())),
            Some(Message::Binary(_)) => {
                Err(Fault::Closed("the server sent a binary message".into()))
            }
        }
    }

    /// Runs `step` in reads of at most `CANCEL_POLL` until it finishes, the
    /// deadline passes, or the flag is set. A deadline already passed still
    /// gets one read that does not wait, and no deadline waits for the flag.
    fn poll<T>(
        &mut self,
        deadline: Option<Instant>,
        limit: Duration,
        mut step: impl FnMut(&mut WebSocket<S>) -> Result<T, WsError>,
    ) -> Result<Option<T>, Fault> {
        let mut read = false;
        loop {
            if self.limits.cancelled.load(Ordering::Relaxed) {
                return Err(Fault::Cancelled);
            }
            let left = deadline.map_or(CANCEL_POLL, |deadline| {
                deadline.saturating_duration_since(Instant::now())
            });
            if left.is_zero() && read {
                return Ok(None);
            }
            read = true;
            self.socket.stream().read_within(left.min(CANCEL_POLL));
            match step(&mut self.socket) {
                Ok(done) => return Ok(Some(done)),
                Err(WsError::Io(error))
                    if timed_out(&error) || error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(fault(error, limit)),
            }
        }
    }
}

impl<S: Timed> Drop for Connection<'_, S> {
    fn drop(&mut self) {
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

fn timeout_cap(value: Option<&str>) -> Duration {
    value
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|milliseconds| *milliseconds > 0)
        .map_or(DEFAULT_TIMEOUT, Duration::from_millis)
        .min(MAX_TIMEOUT)
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
