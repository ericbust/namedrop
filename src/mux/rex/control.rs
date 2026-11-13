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
    fn read_within(&self, timeout: Duration);
    fn write_within(&self, timeout: Duration);
}

/// macOS refuses a socket option once the peer has hung up, though the bytes
/// it sent are still readable. The timeout set before then stays, and the
/// next read or write reports the hang-up itself, so the refusal is ignored.
impl Timed for UnixStream {
    fn read_within(&self, timeout: Duration) {
        let _ = self.set_read_timeout(Some(timeout));
    }

    fn write_within(&self, timeout: Duration) {
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
}

impl<'a, S: Timed> Connection<'a, S> {
    pub(super) fn open(stream: S, limits: Limits<'a>) -> Result<Self, Fault> {
        let timeout_limit = timeout_cap(std::env::var("NAMEDROP_REX_TIMEOUT_MS").ok().as_deref());
        let budget = request_budget(&limits, timeout_limit)?;
        stream.read_within(budget.timeout);
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
        };
        let hello = json!({
            "type": "hello",
            "api_revision": API_REVISION,
            "api_min_revision": API_REVISION,
            "client": {"kind": "other", "name": "namedrop", "version": env!("CARGO_PKG_VERSION")},
        });
        let welcome = connection.exchange(budget, &hello, |_| true)?;
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
        let budget = request_budget(&self.limits, self.timeout_limit)?;
        self.next_id += 1;
        let id = self.next_id.to_string();
        let mut request = json!({"type": "request", "id": id, "method": method});
        if let Some(payload) = payload {
            request["payload"] = payload;
        }
        let response = self.exchange(budget, &request, |message| {
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

    pub(super) fn next_event(&mut self, within: Duration) -> Result<Option<Event>, Fault> {
        if let Some(event) = self.events.pop_front() {
            return Ok(Some(event));
        }
        let deadline = Instant::now() + within;
        while let Some(message) = self.receive(deadline, within)? {
            if message["type"] == "event" {
                return parse_event(&message).map(Some);
            }
        }
        Ok(None)
    }

    fn exchange(
        &mut self,
        budget: Budget,
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
        let answered = self.send_and_wait(budget.timeout, &text, answers);
        let used = held - self.socket.allowance();
        self.socket.set_allowance(left - used);
        answered
    }

    fn send_and_wait(
        &mut self,
        timeout: Duration,
        text: &str,
        answers: impl Fn(&Value) -> bool,
    ) -> Result<Value, Fault> {
        self.socket.stream().write_within(timeout);
        self.socket
            .send_text(text)
            .map_err(|error| fault(error, timeout))?;
        let deadline = Instant::now() + timeout;
        loop {
            let Some(message) = self.receive(deadline, timeout)? else {
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

    fn receive(&mut self, deadline: Instant, limit: Duration) -> Result<Option<Value>, Fault> {
        loop {
            if self.limits.cancelled.load(Ordering::Relaxed) {
                return Err(Fault::Cancelled);
            }
            let Some(left) = deadline
                .checked_duration_since(Instant::now())
                .filter(|left| !left.is_zero())
            else {
                return Ok(None);
            };
            self.socket.stream().read_within(left.min(CANCEL_POLL));
            match self.socket.recv() {
                Ok(Message::Text(text)) => {
                    let message: Value = serde_json::from_str(&text).map_err(|_| {
                        Fault::Closed("the server sent a message that is not JSON".into())
                    })?;
                    return Ok(Some(message));
                }
                Ok(Message::Binary(_)) => {
                    return Err(Fault::Closed("the server sent a binary message".into()));
                }
                Err(WsError::Io(error)) if timed_out(&error) => {}
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
mod tests;
