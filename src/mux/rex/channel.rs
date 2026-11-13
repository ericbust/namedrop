//! The gate's channel on one Rex control connection: the tap, the facts,
//! the paste, the re-read, and the Enter, ordered by the connection's `seq`.

use super::{
    Call,
    agent::{Locality, Observed, pane_agent},
    binding::{Binding, Bindings},
    control::Event,
    wire::{self, ProcessReply, block_call, terminal_method},
};
use crate::mux::{
    AgentIdentity, AgentState, Gone, MuxError, PaneId,
    gate::{ChannelError, RawChannel, StatusTap, TapChange},
    processes::{LocalProcesses, LocalRead},
};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::VecDeque,
    rc::Rc,
    time::{Duration, Instant},
};

/// What the facts saw and what the paste was acknowledged at, shared by the
/// channel and its tap.
#[derive(Default)]
struct Seen {
    /// The `seq` of the `program_status` response. Events at or below it
    /// are already in the facts.
    boundary: u64,
    /// The `seq` of the Enter's acknowledgement, once it was pressed. A
    /// change at or below it was queued before the Enter started.
    entered: Option<u64>,
    foreground: Option<u32>,
    /// The process whose life the facts admitted: the bound agent, or the
    /// foreground when no binding names it.
    watched: Option<Watched>,
    /// Events the channel read after the paste, which the tap reads next.
    pending: VecDeque<Event>,
}

#[derive(Clone)]
struct Watched {
    pid: u32,
    incarnation: String,
    group: u32,
}

pub(super) struct RexChannel<'a, 'w> {
    call: Rc<RefCell<Call<'w>>>,
    session: String,
    block: String,
    local: &'a dyn LocalProcesses,
    bindings: &'a Bindings,
    seen: Rc<RefCell<Seen>>,
}

impl<'a, 'w> RexChannel<'a, 'w> {
    /// A channel on a connection already attached to `session`, whose
    /// events from then on buffer on the connection.
    pub(super) fn new(
        call: Call<'w>,
        session: String,
        block: String,
        local: &'a dyn LocalProcesses,
        bindings: &'a Bindings,
    ) -> Self {
        Self {
            call: Rc::new(RefCell::new(call)),
            session,
            block,
            local,
            bindings,
            seen: Rc::default(),
        }
    }

    fn block_method(&self, method: &str) -> Result<(u64, Value), ChannelError> {
        let payload = block_call(&self.session, &self.block, json!({}));
        let reply = self
            .call
            .borrow_mut()
            .control
            .request(&terminal_method(method), Some(payload))
            .map_err(|fault| ChannelError(wire::mux_error(fault, Gone::Pane).to_string()))?;
        Ok((reply.seq, reply.payload))
    }

    fn event(&self, event: Value) -> Result<u64, ChannelError> {
        let payload = json!({"session_id": self.session, "block_id": self.block, "event": event});
        self.call
            .borrow_mut()
            .control
            .request("block.event", Some(payload))
            .map(|reply| reply.seq)
            .map_err(|fault| ChannelError(wire::mux_error(fault, Gone::Pane).to_string()))
    }

    fn process(&self) -> Result<ProcessReply, ChannelError> {
        let (_, payload) = self.block_method("process")?;
        wire::parse(payload).map_err(|error: MuxError| ChannelError(error.to_string()))
    }
}

/// Every control character turned into a space, so no pasted line runs
/// before the Enter: without bracketed paste, bash 3.2 and dash run each
/// pasted line that ends in a newline, and a `\r` runs too.
pub(super) fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_ascii_control() { ' ' } else { c })
        .collect()
}

impl<'a, 'w> RawChannel for RexChannel<'a, 'w> {
    type Tap = RexTap<'w>;

    /// The connection was attached before the channel was made, so the tap
    /// only shares it.
    fn tap(&self, _pane: &PaneId) -> Result<RexTap<'w>, ChannelError> {
        Ok(RexTap {
            call: Rc::clone(&self.call),
            block: self.block.clone(),
            seen: Rc::clone(&self.seen),
        })
    }

    /// `program_status`, whose `seq` is the tap's boundary, then `process`,
    /// then the local reads and the binding. `pane_agent` answers.
    fn facts(
        &self,
        _pane: &PaneId,
    ) -> Result<(Option<AgentIdentity>, Option<AgentState>), ChannelError> {
        let (boundary, status) = self.block_method("program_status")?;
        let record = wire::root_record(status).map_err(|error| ChannelError(error.to_string()))?;
        let process = self.process()?;
        let binding = self.bindings.read(&self.block);
        let host = self.call.borrow().host.clone();
        let locality = Locality::read(
            self.local,
            host.as_deref(),
            host.as_deref(),
            process.foreground.as_ref(),
            binding.as_ref(),
        );
        let watched = watched(&locality, binding.as_ref());
        let agent = pane_agent(Observed {
            record: record.as_ref(),
            foreground: process.foreground.as_ref(),
            binding: binding.as_ref(),
            locality,
        });
        let mut seen = self.seen.borrow_mut();
        seen.boundary = boundary;
        seen.foreground = process.foreground.map(|foreground| foreground.pid);
        seen.watched = watched;
        Ok((agent.agent, agent.state))
    }

    /// The one-line paste and its acknowledgement, `process` again, and one
    /// read of what the connection holds. Each of these stops before the
    /// Enter, so the gate holds the prompt: another foreground, a watched
    /// process that is gone, a zombie, unreadable, restarted, or out of the
    /// foreground's group, or any change for the block that Rex queued after
    /// the facts. The read comes last, just before the Enter, so the Enter
    /// follows it by one request. No sleep.
    fn paste_submit(&self, _pane: &PaneId, text: &str) -> Result<(), ChannelError> {
        let block = self.block.clone();
        self.event(json!({"type": "paste", "payload": {"text": one_line(text)}}))?;
        let foreground = self.process()?.foreground.map(|foreground| foreground.pid);
        let (expected, watched) = {
            let seen = self.seen.borrow();
            (seen.foreground, seen.watched.clone())
        };
        if foreground != expected {
            return Err(ChannelError(format!(
                "the foreground of block {block} changed during the paste"
            )));
        }
        let lives = watched.as_ref().is_some_and(|watched| {
            matches!(self.local.read(watched.pid), LocalRead::Alive(now)
                if now.incarnation.0 == watched.incarnation && now.group == watched.group)
        });
        if !lives {
            return Err(ChannelError(format!(
                "the agent in block {block} is no longer the one the facts admitted"
            )));
        }
        loop {
            let next = self.call.borrow_mut().control.next_event(Duration::ZERO);
            match next {
                Ok(Some(event)) => self.seen.borrow_mut().pending.push_back(event),
                Ok(None) => break,
                Err(fault) => {
                    return Err(ChannelError(wire::mux_error(fault, Gone::Pane).to_string()));
                }
            }
        }
        {
            let seen = self.seen.borrow();
            let changed = seen.pending.iter().any(|event| {
                event.seq > seen.boundary
                    && change(&event.payload, &block, seen.foreground).is_some()
            });
            if changed {
                return Err(ChannelError(format!(
                    "block {block} changed before the Enter"
                )));
            }
        }
        let entered =
            self.event(json!({"type": "key", "payload": key_payload(crate::mux::Key::Enter)}))?;
        self.seen.borrow_mut().entered = Some(entered);
        Ok(())
    }
}

/// The process the facts admitted: the bound agent with its bound start and
/// the foreground's group, or the foreground itself.
fn watched(locality: &Locality, binding: Option<&Binding>) -> Option<Watched> {
    let Locality::Local {
        foreground: LocalRead::Alive(foreground),
        ..
    } = locality
    else {
        return None;
    };
    let bound = binding.filter(|binding| binding.foreground == foreground.incarnation.0);
    let incarnation = bound.map_or(&foreground.incarnation.0, |binding| &binding.agent);
    Some(Watched {
        pid: incarnation.split_once('@')?.0.parse().ok()?,
        incarnation: incarnation.clone(),
        group: foreground.group,
    })
}

/// A change of the block's program or status in an event, as the tap reports
/// it. `process_changed` counts only when the foreground pid differs from the
/// facts', because opening a data connection to the block also sends it.
fn change(event: &Value, block: &str, foreground: Option<u32>) -> Option<TapChange> {
    let inner = &event["payload"]["event"];
    if inner["type"] != "block_event" || inner["payload"]["block_id"] != block {
        return None;
    }
    let fields = &inner["payload"]["payload"];
    match inner["payload"]["name"].as_str()? {
        "program_status_changed" if fields["id"] == "" => Some(TapChange::Status(
            AgentState::parse(fields["record"]["state"].as_str().unwrap_or_default()),
        )),
        "program_status_removed"
            if fields["ids"]
                .as_array()
                .is_some_and(|ids| ids.iter().any(|id| id == "")) =>
        {
            Some(TapChange::Program)
        }
        "child_exited" => Some(TapChange::Program),
        "process_changed" if fields["foreground"]["pid"].as_u64() != foreground.map(u64::from) => {
            Some(TapChange::Program)
        }
        _ => None,
    }
}

pub(super) struct RexTap<'w> {
    call: Rc<RefCell<Call<'w>>>,
    block: String,
    seen: Rc<RefCell<Seen>>,
}

impl StatusTap for RexTap<'_> {
    /// Skips events at or below the facts' boundary and events for other
    /// blocks. A zero wait still reads what the socket holds once. A status
    /// change at or below the Enter's acknowledgement was queued before the
    /// Enter started, so it reports `Program`, which never confirms.
    fn next_change(&mut self, within: Duration) -> Option<TapChange> {
        let deadline = Instant::now().checked_add(within);
        loop {
            let pending = self.seen.borrow_mut().pending.pop_front();
            let event = match pending {
                Some(event) => event,
                None => {
                    let left = deadline.map_or(Duration::MAX, |deadline| {
                        deadline.saturating_duration_since(Instant::now())
                    });
                    match self.call.borrow_mut().control.next_event(left) {
                        Ok(Some(event)) => event,
                        Ok(None) => return None,
                        Err(_) => return Some(TapChange::Program),
                    }
                }
            };
            let seen = self.seen.borrow();
            if event.seq <= seen.boundary {
                continue;
            }
            match change(&event.payload, &self.block, seen.foreground) {
                Some(TapChange::Status(_))
                    if seen.entered.is_some_and(|entered| event.seq <= entered) =>
                {
                    return Some(TapChange::Program);
                }
                Some(change) => return Some(change),
                None => {}
            }
        }
    }
}

/// The `block.event` key payload for each key, as Rex's terminal turns them
/// into bytes: Enter `\r`, Escape `ESC`, Ctrl-C `0x03`, Ctrl-U `0x15`, Up
/// `ESC[A`, and Down `ESC[B` (measured on a scratch block running `od -c`).
pub(super) fn key_payload(key: crate::mux::Key) -> Value {
    use crate::mux::Key;
    let (name, mods, text, code) = match key {
        Key::Enter => ("enter", "", "\r", 13),
        Key::Escape => ("escape", "", "", 27),
        Key::CtrlC => ("key_c", "ctrl", "", 99),
        Key::CtrlU => ("key_u", "ctrl", "", 117),
        Key::Up => ("arrow_up", "", "", 0),
        Key::Down => ("arrow_down", "", "", 0),
    };
    json!({"key": name, "mods": mods, "consumed_mods": "", "text": text,
           "unshifted_codepoint": code, "composing": false, "action": "press"})
}

#[cfg(test)]
mod tests;
