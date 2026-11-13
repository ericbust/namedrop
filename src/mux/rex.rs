//! The Rex adapter: the only place that holds `rex.control.v1` methods,
//! Rex JSON, and Rex error codes. Each port call opens one control
//! connection, proves the server is on this machine, runs its requests, and
//! closes it.

mod agent;
mod binding;
mod channel;
mod control;
#[cfg(test)]
mod live;
mod local;
pub(crate) mod wire;
mod ws;

use super::{
    Allowance, Applied, Budget, Depth, Effect, Endpoint, Gone, InputKind, InputOutcome, Key,
    MuxAct, MuxError, MuxInput, MuxRead, PaneFacts, PaneId, PaneInput, PaneProcess, PromptRequest,
    Retry, Scope, Screen, ScreenRequest, SpaceView, Style, Survey, TabId, TerminalId, Wrap, gate,
    narrowest,
    processes::{LocalProcesses, LocalRead, members},
};
use crate::{SpaceId, WorktreeId};
use agent::{Locality, Observed, pane_agent, shell_of};
use binding::Bindings;
use control::{Connection, Control, Fault, Limits, Narrower, Wire};
use serde_json::{Value, json};
use std::{
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::Duration,
};
use wire::{BlockEntry, ProcessReply, TERMINAL, block_call, mux_error, terminal_method};

/// Opens nothing. Each port call of the connection dials the endpoint's
/// socket, proves the server local, runs, and closes, and reads session
/// bindings from `state`.
pub(super) fn connect(
    state: &Path,
    endpoint: &Endpoint,
    budget: Budget,
    cancelled: Arc<AtomicBool>,
) -> Result<Arc<dyn super::Mux>, MuxError> {
    let path = endpoint.unix_socket().ok_or_else(|| {
        MuxError::Unavailable(format!(
            "{} names no Rex server's Unix socket",
            endpoint.key
        ))
    })?;
    let socket = Socket::new(path.to_owned(), budget, cancelled);
    let local = Arc::new(super::processes::ThisMachine);
    Ok(Arc::new(RexMux::new(
        socket,
        endpoint.clone(),
        state,
        local,
    )))
}

/// The Unix socket a `rex:` endpoint names, dialed afresh for each port call.
struct Socket {
    path: PathBuf,
    budget: Budget,
    cancelled: Arc<AtomicBool>,
    /// `NAMEDROP_REX_TIMEOUT_MS` when it is set. It caps each request, and
    /// the Stop wrapper sets it to 250 ms, so a process that carries it
    /// types no prompt it could not confirm.
    set_cap: Option<Duration>,
}

impl Socket {
    fn new(path: PathBuf, budget: Budget, cancelled: Arc<AtomicBool>) -> Self {
        Self {
            path,
            budget,
            cancelled,
            set_cap: control::explicit_timeout_cap(),
        }
    }
}

impl Wire for Socket {
    fn open<'a>(&'a self, narrower: Narrower<'a>) -> Result<(Box<dyn Control + 'a>, u32), Fault> {
        let cancelled = narrowest(narrower.cancelled, Some(&self.cancelled))
            .map_err(|_| Fault::Cancelled)?
            .unwrap_or(&self.cancelled);
        let (ask, own) = (narrower.budget, self.budget);
        let limits = Limits {
            budget: Box::new(move || Ok(ask()?.tighter(own))),
            cancelled,
        };
        let stream = UnixStream::connect(&self.path).map_err(|error| {
            Fault::Closed(format!(
                "could not reach Rex at {}: {error}",
                self.path.display()
            ))
        })?;
        let peer = local::peer_pid(&stream).map_err(|error| {
            Fault::NotLocal(format!(
                "{} cannot be proved a Rex server on this machine: {error}",
                self.path.display()
            ))
        })?;
        Ok((Box::new(Connection::open(stream, limits)?), peer))
    }

    /// The budget's timeout, held to `NAMEDROP_REX_TIMEOUT_MS` only when it
    /// is set. Its 5 s default is left out, or no prompt would fit.
    fn timeout(&self) -> Result<Duration, String> {
        Ok(self
            .set_cap
            .map_or(self.budget.timeout, |cap| self.budget.timeout.min(cap)))
    }
}

/// A wire whose every request is held to `allowance` as well.
struct Within<'a, W> {
    wire: W,
    allowance: Allowance<'a>,
}

impl<W: Wire> Wire for Within<'_, W> {
    /// The allowance's budget is asked for first, so its error wins, as it
    /// does on Herdr.
    fn open<'b>(&'b self, narrower: Narrower<'b>) -> Result<(Box<dyn Control + 'b>, u32), Fault> {
        let cancelled = narrowest(narrower.cancelled, self.allowance.cancelled())
            .map_err(|_| Fault::Cancelled)?;
        let ask = narrower.budget;
        self.wire.open(Narrower {
            budget: Box::new(move || Ok(self.allowance.budget()?.tighter(ask()?))),
            cancelled,
        })
    }

    fn timeout(&self) -> Result<Duration, String> {
        Ok(self.allowance.budget()?.timeout.min(self.wire.timeout()?))
    }
}

#[derive(Clone, Debug)]
struct Proven {
    pid: u32,
    host_id: Option<String>,
}

struct RexMux<W> {
    wire: W,
    endpoint: Endpoint,
    bindings: Bindings,
    local: Arc<dyn LocalProcesses>,
    /// The server the last local proof saw, shared with every `within`.
    proven: Arc<Mutex<Option<Proven>>>,
}

/// One port call's connection, proved local.
struct Call<'a> {
    control: Box<dyn Control + 'a>,
    /// The server's `host_id`, which a local block shares.
    host: Option<String>,
}

impl Call<'_> {
    fn request(&mut self, method: &str, payload: Value, gone: Gone) -> Result<Value, MuxError> {
        self.control
            .request(method, Some(payload))
            .map(|reply| reply.payload)
            .map_err(|fault| mux_error(fault, gone))
    }

    fn block(
        &mut self,
        session: &str,
        block: &str,
        method: &str,
        args: Value,
    ) -> Result<Value, MuxError> {
        self.request(
            &terminal_method(method),
            block_call(session, block, args),
            Gone::Pane,
        )
    }

    fn sessions(&mut self) -> Result<Vec<wire::SessionEntry>, MuxError> {
        wire::sessions(self.request("session.list", json!({}), Gone::Space)?)
    }

    fn attach(&mut self, session: &str) -> Result<Value, MuxError> {
        self.request(
            "session.attach",
            json!({"session_id": session}),
            Gone::Space,
        )
    }

    fn blocks(&mut self, session: &str) -> Result<Vec<BlockEntry>, MuxError> {
        wire::blocks(self.request(
            "session.list_blocks",
            json!({"session_id": session}),
            Gone::Space,
        )?)
    }

    /// The session that holds `id`, a block or a window. An empty ID is
    /// refused, because Rex resolves it to its default window, and a
    /// detached block's empty tab ID names no window.
    fn resolve(&mut self, kind: &str, id: &str, gone: Gone) -> Result<wire::Target, MuxError> {
        if id.is_empty() {
            return Err(MuxError::Gone(
                gone,
                format!("Rex has no {kind} with an empty ID"),
            ));
        }
        let payload = self.request("target.resolve", wire::resolve_request(kind, id), gone)?;
        wire::target(payload, gone, id)
    }

    /// The session that holds `block`, attached, and the session's view.
    fn attach_block(&mut self, block: &str) -> Result<(String, Value), MuxError> {
        let session = self
            .resolve("block", block, Gone::Pane)?
            .session_id
            .ok_or_else(|| MuxError::Protocol(format!("Rex named no session for {block}")))?;
        let view = self.attach(&session)?;
        Ok((session, view))
    }

    /// The block's host, from the view or, for a detached block the view
    /// leaves out, from the block list.
    fn block_host(
        &mut self,
        session: &str,
        view: &Value,
        block: &str,
    ) -> Result<Option<String>, MuxError> {
        let host = match wire::view_host(view, block) {
            Some(host) => host,
            None => {
                self.blocks(session)?
                    .into_iter()
                    .find(|entry| entry.block_id == block)
                    .ok_or_else(|| MuxError::Gone(Gone::Pane, format!("Rex has no {block}")))?
                    .host_id
            }
        };
        Ok(host)
    }
}

impl<W: Wire> RexMux<W> {
    fn new(wire: W, endpoint: Endpoint, state: &Path, local: Arc<dyn LocalProcesses>) -> Self {
        Self {
            wire,
            endpoint,
            bindings: Bindings::new(state),
            local,
            proven: Arc::default(),
        }
    }

    /// A connection whose peer is the server the last proof saw, or one
    /// proved again with `server.status`, as after a restart.
    fn call(&self) -> Result<Call<'_>, MuxError> {
        let unavailable = |fault| mux_error(fault, Gone::Pane);
        let (mut control, peer) = self.wire.open(Narrower::none()).map_err(unavailable)?;
        let proven = self.proven.lock().ok().and_then(|proven| proven.clone());
        let host = match proven {
            Some(proven) if proven.pid == peer => proven.host_id,
            _ => {
                let dial = self
                    .endpoint
                    .unix_socket()
                    .unwrap_or(Path::new(&self.endpoint.key));
                let server = local::prove_local(dial, &mut *control, peer).map_err(unavailable)?;
                // A stored endpoint is the canonical one its server reported,
                // so another one means another server took the socket.
                if server.endpoint != self.endpoint {
                    return Err(MuxError::Unavailable(format!(
                        "{} is now served by the Rex server whose endpoint is {}",
                        self.endpoint.key, server.endpoint.key
                    )));
                }
                if let Ok(mut proven) = self.proven.lock() {
                    *proven = Some(Proven {
                        pid: server.pid,
                        host_id: server.host_id.clone(),
                    });
                }
                server.host_id
            }
        };
        Ok(Call { control, host })
    }

    fn panes(&self, call: &mut Call<'_>, session: &str) -> Result<Vec<PaneFacts>, MuxError> {
        call.attach(session)?;
        let mut panes = Vec::new();
        for entry in call.blocks(session)? {
            if entry.creator_name.as_deref() != Some(TERMINAL) {
                continue;
            }
            match self.pane(call, session, entry) {
                Ok(pane) => panes.push(pane),
                // The block closed after the listing.
                Err(MuxError::Gone(Gone::Pane, _)) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(panes)
    }

    fn pane(
        &self,
        call: &mut Call<'_>,
        session: &str,
        entry: BlockEntry,
    ) -> Result<PaneFacts, MuxError> {
        let block = entry.block_id.as_str();
        let record = wire::root_record(call.block(session, block, "program_status", json!({}))?)?;
        let process: ProcessReply =
            wire::parse(call.block(session, block, "process", json!({}))?)?;
        let binding = self.bindings.read(block);
        let locality = Locality::read(
            &*self.local,
            entry.host_id.as_deref(),
            call.host.as_deref(),
            process.foreground.as_ref(),
            binding.as_ref(),
        );
        let agent = pane_agent(Observed {
            record: record.as_ref(),
            foreground: process.foreground.as_ref(),
            binding: binding.as_ref(),
            locality,
        });
        Ok(PaneFacts {
            id: PaneId::new(block),
            terminal: TerminalId::new(block),
            tab: TabId::new(entry.window_id.unwrap_or_default()),
            space: SpaceId::new(session),
            cwd: process.foreground.and_then(|foreground| foreground.cwd),
            title: Some(entry.label).filter(|label| !label.is_empty()),
            agent: agent.agent,
            state: agent.state,
            idle_label: None,
            viewport_rows: None,
            reports_session: agent.reports_session,
        })
    }

    /// The space a session is, with its list position as its number.
    fn space(session: wire::SessionEntry, position: usize) -> SpaceView {
        SpaceView {
            id: SpaceId::new(&session.session_id),
            label: Some(session.label),
            number: u32::try_from(position + 1).ok(),
            checkout: None,
            worktree: None,
            panes: Vec::new(),
        }
    }

    fn listed_space(call: &mut Call<'_>, space: &SpaceId) -> Result<SpaceView, MuxError> {
        call.sessions()?
            .into_iter()
            .enumerate()
            .find(|(_, session)| session.session_id == space.as_str())
            .map(|(position, session)| Self::space(session, position))
            .ok_or_else(|| {
                MuxError::Gone(
                    Gone::Space,
                    format!("Rex has no session {}", space.as_str()),
                )
            })
    }
}

/// The key Namedrop stores a session's state under, since a Rex session is
/// not a checkout.
fn space_key(space: &SpaceId) -> WorktreeId {
    WorktreeId::new(format!("rex-space:{}", space.as_str()))
}

/// The last `lines` lines of a screen read from `offset`, in the port's
/// shape. A read that starts after the first byte starts inside a line, which
/// is dropped before the text is parsed, because the cut can fall inside an
/// escape sequence.
fn screen_lines(text: &str, offset: u64, request: ScreenRequest) -> Result<String, MuxError> {
    let text = match offset {
        0 => text,
        _ => text.split_once('\n').map_or("", |(_, rest)| rest),
    };
    let text = match request.style {
        Style::Ansi => wire::port_ansi(text).map_err(|sequence| {
            MuxError::Unavailable(format!(
                "Rex's screen holds an escape sequence Namedrop does not read: {sequence}"
            ))
        })?,
        Style::Plain => text.replace("\r\n", "\n"),
    };
    let lines: Vec<&str> = text.split('\n').collect();
    let start = lines.len().saturating_sub(request.lines);
    Ok(lines[start..].join("\n"))
}

/// How far back from the end a screen read starts. Even a screen that is all
/// escapes encodes within one 960 KiB `blob_read`.
const SCREEN_TAIL: u64 = 160 * 1024;
const BLOB_READ_MAX: u64 = 983_040;

impl<W: Wire> MuxRead for RexMux<W> {
    fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    fn survey(&self, scope: Scope<'_>, depth: Depth) -> Result<Survey, MuxError> {
        if let Scope::CheckoutOf(_) = scope {
            return Err(MuxError::Unavailable(
                "a Rex session is not a checkout".into(),
            ));
        }
        let mut call = self.call()?;
        let spaces = match (scope, depth) {
            (Scope::CheckoutOf(_), _) => unreachable!("refused before the connection"),
            (Scope::All, Depth::Spaces) => call
                .sessions()?
                .into_iter()
                .enumerate()
                .map(|(position, session)| Self::space(session, position))
                .collect(),
            (Scope::Space(space), Depth::Spaces) => vec![Self::listed_space(&mut call, space)?],
            (Scope::Space(space), Depth::Panes) => {
                let mut view = Self::listed_space(&mut call, space)?;
                view.worktree = Some(space_key(space));
                view.panes = self.panes(&mut call, space.as_str())?;
                vec![view]
            }
            (Scope::PanesOf(space), _) => vec![SpaceView {
                id: space.clone(),
                label: None,
                number: None,
                checkout: None,
                worktree: None,
                panes: self.panes(&mut call, space.as_str())?,
            }],
            (Scope::All, Depth::Panes) => {
                let sessions = call.sessions()?;
                let mut spaces = Vec::new();
                for session in sessions {
                    let id = SpaceId::new(&session.session_id);
                    match self.panes(&mut call, id.as_str()) {
                        Ok(panes) => spaces.push(SpaceView {
                            id,
                            label: None,
                            number: None,
                            checkout: None,
                            worktree: None,
                            panes,
                        }),
                        // The session ended after the listing.
                        Err(MuxError::Gone(Gone::Space, _)) => {}
                        Err(error) => return Err(error),
                    }
                }
                spaces
            }
        };
        Ok(Survey { spaces })
    }

    /// `format` with a one-byte limit names the whole result's `id` and
    /// `total`, and `blob_read` takes its tail, read on until `eof`, because
    /// a read can stop short of the end.
    fn screen(&self, pane: &PaneId, request: ScreenRequest) -> Result<Screen, MuxError> {
        let mut call = self.call()?;
        let block = pane.as_str();
        let (session, _) = call.attach_block(block)?;
        let format = match request.style {
            Style::Ansi => "vt",
            Style::Plain => "text",
        };
        let args =
            json!({"format": format, "unwrap": request.wrap == Wrap::Joined, "max_bytes": 1});
        let first = call.block(&session, block, "format", args)?;
        let (Some(id), Some(total)) = (first["id"].as_u64(), first["total"].as_u64()) else {
            let content = first["content"].as_str().unwrap_or_default();
            return Ok(Screen {
                text: screen_lines(content, 0, request)?,
            });
        };
        let offset = total.saturating_sub(SCREEN_TAIL);
        let mut data = String::new();
        loop {
            let at = offset + data.len() as u64;
            let args = json!({"id": id, "offset": at, "max_bytes": BLOB_READ_MAX});
            let tail = call.block(&session, block, "blob_read", args)?;
            let part = tail["data"].as_str().ok_or_else(|| {
                MuxError::Protocol(format!("Rex's blob_read holds no data: {tail}"))
            })?;
            data.push_str(part);
            if tail["eof"] == true {
                break;
            }
            if part.is_empty() {
                return Err(MuxError::Protocol(format!(
                    "Rex's blob_read stopped before the end of the screen: {tail}"
                )));
            }
        }
        Ok(Screen {
            text: screen_lines(&data, offset, request)?,
        })
    }

    fn process(&self, pane: &PaneId) -> Result<PaneProcess, MuxError> {
        let mut call = self.call()?;
        let block = pane.as_str();
        let (session, view) = call.attach_block(block)?;
        let host = call.block_host(&session, &view, block)?;
        let reply: ProcessReply =
            wire::parse(call.block(&session, block, "process", json!({}))?)?;
        if host.is_none() || host != call.host {
            return Ok(PaneProcess::default());
        }
        let group = match reply
            .foreground
            .as_ref()
            .map(|foreground| self.local.read(foreground.pid))
        {
            Some(LocalRead::Alive(foreground)) => Some(foreground.group),
            _ => None,
        };
        let foreground_pids = group.and_then(|group| members(&*self.local, group));
        Ok(PaneProcess {
            shell_pid: shell_of(&reply, foreground_pids.as_deref()),
            foreground_group: group,
            foreground_pids: foreground_pids.unwrap_or_default(),
            tty: None,
        })
    }

    fn tab_label(&self, tab: &TabId) -> Result<Option<String>, MuxError> {
        let target = self.call()?.resolve("window", tab.as_str(), Gone::Tab)?;
        Ok(Some(target.label).filter(|label| !label.is_empty()))
    }

    /// Rex has no plugin host, so no integration is installed in it.
    fn integration_root(&self) -> Result<Option<PathBuf>, MuxError> {
        Ok(None)
    }

    fn within<'a>(&'a self, allowance: Allowance<'a>) -> Box<dyn MuxRead + 'a> {
        let wire: &dyn Wire = &self.wire;
        Box::new(RexMux {
            wire: Within { wire, allowance },
            endpoint: self.endpoint.clone(),
            bindings: self.bindings.clone(),
            local: Arc::clone(&self.local),
            proven: Arc::clone(&self.proven),
        })
    }
}

/// Why typing into a block cannot start, in the words of the outcome that
/// leaves the input untyped.
fn untyped(error: MuxError) -> InputOutcome {
    match error {
        MuxError::Gone(_, text) => InputOutcome::Declined(Retry::OnEvent, text),
        error => InputOutcome::Declined(Retry::AfterDelay, error.to_string()),
    }
}

impl<W: Wire> RexMux<W> {
    /// A connection attached to the block's session, refused for a block
    /// on another machine, whose pids name nothing here.
    fn typing_call(&self, block: &str) -> Result<(Call<'_>, String), InputOutcome> {
        let mut call = self.call().map_err(untyped)?;
        let (session, view) = call.attach_block(block).map_err(untyped)?;
        let host = call.block_host(&session, &view, block).map_err(untyped)?;
        if host.is_none() || host != call.host {
            return Err(InputOutcome::Refused(format!(
                "block {block} runs on another machine, and Namedrop types only into blocks on \
                 this one"
            )));
        }
        Ok((call, session))
    }

    fn prompt(&self, pane: &PaneId, request: &PromptRequest<'_>) -> InputOutcome {
        let needed = gate::CONFIRM_WINDOW + Duration::from_secs(1);
        match self.wire.timeout() {
            Err(error) => return InputOutcome::Declined(Retry::AfterDelay, error),
            Ok(timeout) if timeout < needed => {
                return InputOutcome::Declined(
                    Retry::AfterDelay,
                    format!(
                        "a prompt needs {}s to confirm, and this connection allows {}ms",
                        needed.as_secs(),
                        timeout.as_millis()
                    ),
                );
            }
            Ok(_) => {}
        }
        let (call, session) = match self.typing_call(pane.as_str()) {
            Ok(typing) => typing,
            Err(outcome) => return outcome,
        };
        let channel = channel::RexChannel::new(
            call,
            session,
            pane.as_str().to_owned(),
            &*self.local,
            &self.bindings,
        );
        gate::emulate_prompt(&channel, pane, request)
    }

    /// Input with no confirmation: each `block.event` in turn. A Rex error
    /// on the first one typed nothing, unless it is `timeout`, whose work may
    /// already have happened, `internal`, or a 5xx status, which say nothing
    /// either way. Any other failure may have typed.
    fn type_events(&self, pane: &PaneId, events: Vec<Value>) -> InputOutcome {
        let block = pane.as_str();
        let (mut call, session) = match self.typing_call(block) {
            Ok(typing) => typing,
            Err(outcome) => return outcome,
        };
        for (index, event) in events.into_iter().enumerate() {
            let payload = json!({"session_id": session, "block_id": block, "event": event});
            match call.control.request("block.event", Some(payload)) {
                Ok(_) => {}
                Err(fault @ Fault::Rex { .. }) if index == 0 && untyped_by(&fault) => {
                    return InputOutcome::Refused(mux_error(fault, Gone::Pane).to_string());
                }
                Err(fault) => {
                    return InputOutcome::Ambiguous(format!(
                        "input may have reached block {block}: {}",
                        mux_error(fault, Gone::Pane)
                    ));
                }
            }
        }
        InputOutcome::Delivered
    }
}

/// A Rex error that says its request did nothing.
fn untyped_by(fault: &Fault) -> bool {
    matches!(fault, Fault::Rex { code, status, .. }
        if *status < 500 && !matches!(code.as_str(), "timeout" | "internal"))
}

impl<W: Wire> MuxInput for RexMux<W> {
    fn deliver(&self, input: PaneInput<'_>) -> InputOutcome {
        self.send(input.pane(), input.kind())
    }
}

impl<W: Wire> RexMux<W> {
    /// `MuxInput::deliver` minus the lock token.
    fn send(&self, pane: &PaneId, kind: InputKind<'_>) -> InputOutcome {
        match kind {
            InputKind::Prompt(request) => self.prompt(pane, &request),
            // Rewriting a multi-line command into one line would run
            // something other than what was asked.
            InputKind::Line(line) if line.contains(['\n', '\r']) => InputOutcome::Refused(format!(
                "a terminal line for block {} holds a line break",
                pane.as_str()
            )),
            InputKind::Line(line) => self.type_events(
                pane,
                vec![
                    json!({"type": "paste", "payload": {"text": channel::one_line(line)}}),
                    json!({"type": "key", "payload": channel::key_payload(Key::Enter)}),
                ],
            ),
            InputKind::Keys { keys, .. } => self.type_events(
                pane,
                keys.iter()
                    .map(|key| json!({"type": "key", "payload": channel::key_payload(*key)}))
                    .collect(),
            ),
        }
    }
}

impl<W: Wire> MuxAct for RexMux<W> {
    /// `Identify` labels the block `@alias`, and `RenameTab` labels the
    /// window. Rex shows no badges, notifications, or popups yet, and has no
    /// keybindings to reload.
    fn apply(&self, effect: &Effect) -> Result<Applied, MuxError> {
        match effect {
            Effect::Identify { pane, alias, .. } => {
                let mut call = self.call()?;
                let (session, _) = call.attach_block(pane.as_str())?;
                let label = json!({"session_id": session, "block_id": pane.as_str(),
                                   "label": format!("@{alias}")});
                call.request("session.set_block_label", label, Gone::Pane)?;
                Ok(Applied::Done)
            }
            Effect::RenameTab { tab, label } => {
                let mut call = self.call()?;
                let session = call
                    .resolve("window", tab.as_str(), Gone::Tab)?
                    .session_id
                    .ok_or_else(|| {
                        MuxError::Protocol(format!("Rex named no session for {}", tab.as_str()))
                    })?;
                call.attach(&session)?;
                let label =
                    json!({"session_id": session, "window_id": tab.as_str(), "label": label});
                call.request("session.set_window_label", label, Gone::Tab)?;
                Ok(Applied::Done)
            }
            Effect::Badges { .. }
            | Effect::Notify { .. }
            | Effect::OpenPopup(_)
            | Effect::ReloadKeybindings => Ok(Applied::Unsupported),
        }
    }
}

#[cfg(test)]
mod tests;
