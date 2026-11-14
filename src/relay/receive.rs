//! B's bridge as the relay's receiver: it admits each call of
//! `namedrop.relay`, hands it to `namedrop relay accept`, and answers it
//! when the accept exits, or with `retry` when it takes too long. The event
//! loop never waits for an accept, and a thread reaps each one as it exits.

use super::wire::{Answer, Outcome, Status};
use crate::{
    daemon::{self, Scope, Starter},
    locator::{Locator, Name},
    mux::{Endpoint, rex::events::Perform},
};
use serde_json::Value;
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// How long the bridge waits for an accept before it answers `retry`.
pub(crate) const ANSWER_WITHIN: Duration = Duration::from_secs(5);
/// How often the bridge looks for a finished accept while one runs.
pub(crate) const TICK: Duration = Duration::from_millis(20);

/// How an accept starts: a command whose stdin takes the call's arguments.
pub(crate) type Start = Box<dyn Fn(&str) -> Result<Command, String>>;

pub(crate) struct Receiver {
    state: PathBuf,
    start: Start,
    within: Duration,
    calls: Vec<Call>,
    /// Accepts already answered `retry`, which finish their commit.
    late: Vec<Accept>,
}

/// A running accept's thread, which reads its output, reaps it, and then
/// returns the output.
type Accept = JoinHandle<Vec<u8>>;

struct Call {
    id: String,
    item: (String, String),
    host_id: String,
    accept: Accept,
    until: Instant,
}

/// One answer for the stream to send: a call id and its result.
pub(crate) type Reply = (String, Value);

impl Receiver {
    /// The bridge's receiver for `endpoint`, whose accepts run this
    /// Namedrop with `state`.
    pub(crate) fn new(state: &Path, endpoint: &Endpoint) -> Self {
        let (state_owned, endpoint) = (state.to_owned(), endpoint.clone());
        Self::with(
            state,
            Box::new(move |host_id| {
                Ok(accept_command(
                    &crate::runtime_executable()?,
                    &state_owned,
                    &endpoint,
                    host_id,
                ))
            }),
            ANSWER_WITHIN,
        )
    }

    pub(crate) fn with(state: &Path, start: Start, within: Duration) -> Self {
        Self {
            state: state.to_owned(),
            start,
            within,
            calls: Vec::new(),
            late: Vec::new(),
        }
    }

    /// Whether a call waits for its accept, so the bridge looks again within
    /// [`TICK`].
    pub(crate) fn busy(&self) -> bool {
        !self.calls.is_empty()
    }

    /// Admits `perform` for the server whose `host_id` is `host_id`, and
    /// starts its accept. A call refused here, or one whose accept could not
    /// start, is answered at once, and nothing is stored for it.
    pub(crate) fn receive(&mut self, perform: Perform, host_id: Option<&str>) -> Option<Reply> {
        let host_id = host_id.unwrap_or_default().to_owned();
        let item = (
            text(&perform.args["item"]["id"]),
            text(&perform.args["item"]["to"]),
        );
        let answer = |status, reason: String| {
            Some((
                perform.call_id.clone(),
                answer(&host_id, &item, status, &reason),
            ))
        };
        if let Err(reason) = admit(&self.state, &perform.principal) {
            return answer(Status::Refused, reason);
        }
        if perform.args["expect_host_id"] != host_id.as_str() {
            return answer(Status::Refused, "wrong_host".to_owned());
        }
        let started = (self.start)(&host_id).and_then(|mut command| {
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|error| error.to_string())
        });
        let mut child = match started {
            Ok(child) => child,
            Err(error) => return answer(Status::Retry, error),
        };
        if let Some(mut stdin) = child.stdin.take() {
            let input = perform.args.to_string();
            thread::spawn(move || stdin.write_all(input.as_bytes()));
        }
        let accept = thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(stdout) = child.stdout.as_mut() {
                let _ = stdout.read_to_end(&mut bytes);
            }
            let _ = child.wait();
            bytes
        });
        self.calls.push(Call {
            id: perform.call_id,
            item,
            host_id,
            accept,
            until: Instant::now() + self.within,
        });
        None
    }

    /// The answers whose accept finished, and `retry` for each whose time
    /// ran out by `now`, whose accept still finishes its commit.
    pub(crate) fn poll(&mut self, now: Instant) -> Vec<Reply> {
        self.late.retain(|accept| !accept.is_finished());
        let mut replies = Vec::new();
        let mut index = 0;
        while index < self.calls.len() {
            let call = &self.calls[index];
            let exited = call.accept.is_finished();
            if !exited && now < call.until {
                index += 1;
                continue;
            }
            let call = self.calls.swap_remove(index);
            if exited {
                let output = call.accept.join().unwrap_or_default();
                let result = serde_json::from_slice::<Answer>(&output)
                    .ok()
                    .and_then(|answer| serde_json::to_value(answer).ok())
                    .unwrap_or_else(|| {
                        answer(
                            &call.host_id,
                            &call.item,
                            Status::Retry,
                            "the relay's accept gave no answer",
                        )
                    });
                replies.push((call.id, result));
            } else {
                replies.push((
                    call.id,
                    answer(&call.host_id, &call.item, Status::Retry, "still deciding"),
                ));
                self.late.push(call.accept);
            }
        }
        replies
    }

    /// Lets each running accept finish unanswered once its connection is
    /// gone, since no other connection can answer its call.
    pub(crate) fn abandon(&mut self) {
        for call in self.calls.drain(..) {
            self.late.push(call.accept);
        }
    }

    /// Waits for every accept still running, so none outlives the bridge
    /// unreaped.
    pub(crate) fn finish(&mut self) {
        self.abandon();
        for accept in self.late.drain(..) {
            let _ = accept.join();
        }
    }
}

/// `namedrop relay accept` for the server `host_id` behind `endpoint`.
fn accept_command(executable: &Path, state: &Path, endpoint: &Endpoint, host_id: &str) -> Command {
    let mut command = daemon::command(executable, Scope::Endpoint(endpoint), Starter::Caller);
    command.args([
        "relay",
        "accept",
        "--host-id",
        host_id,
        "--endpoint",
        &endpoint.key,
    ]);
    Locator::StateDir.pass(&mut command, Name::of(endpoint.kind), state);
    command
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

fn answer(host_id: &str, item: &(String, String), status: Status, reason: &str) -> Value {
    serde_json::to_value(Answer {
        host_id: host_id.to_owned(),
        can_reply: status == Status::Retry,
        result: Some(Outcome {
            id: item.0.clone(),
            to: item.1.clone(),
            status,
            reason: Some(reason.to_owned()),
        }),
        outbox: Vec::new(),
        more: false,
    })
    .unwrap_or(Value::Null)
}

/// Whether B relays for `principal`: a peer with B's own uid, a tailnet
/// principal, which Rex admits only for the device owner unless the owner
/// widened it, and an anonymous one only while `<state>/relay/allow-anonymous`
/// exists, which only the stand-in creates.
fn admit(state: &Path, principal: &Value) -> Result<(), String> {
    match principal["kind"].as_str() {
        // SAFETY: `getuid` has no preconditions.
        Some("peer") if principal["uid"].as_u64() == Some(u64::from(unsafe { libc::getuid() })) => {
            Ok(())
        }
        Some("tailscale") => Ok(()),
        Some("anonymous") if state.join("relay/allow-anonymous").exists() => Ok(()),
        Some(kind) => Err(format!("the relay does not accept a {kind} caller here")),
        None => Err("the relay does not accept a caller with no principal".to_owned()),
    }
}

#[cfg(test)]
mod tests;
