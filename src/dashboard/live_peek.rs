//! The private, receive-only terminal projection boundary for one HTTP viewer.
use super::{
    Front, Request, SECURITY_HEADERS, Status, decode_query_value, respond_status,
    snapshot::{self, SpaceKey},
    strip_terminal_sequences,
};
use crate::{
    HerdrCommandExecutor, ProcessHerdr, StateStore,
    store::dashboard::{LiveBinding, LiveMember},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    fs::OpenOptions,
    io::{self, Read, Write},
    net::TcpStream,
    os::{
        fd::{AsRawFd, RawFd},
        unix::{fs::OpenOptionsExt, process::CommandExt},
    },
    path::{Component, Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const ENCODED_LIMIT: usize = 2 * 1024 * 1024;
const PAINT_LIMIT: usize = 1024 * 1024;
const DATA_LIMIT: usize = 64 * 1024;
const PENDING_LIMIT: usize = 2 * 1024 * 1024;
const STARTUP: Duration = Duration::from_secs(5);
const STALL: Duration = Duration::from_secs(2);
const HEARTBEAT: Duration = Duration::from_secs(10);
const INPUT_LIMIT: usize = 8 * 1024;
const TEXT_LIMIT: usize = 4 * 1024;
const INPUT_RATE: usize = 12;
const HISTORY_LINES: usize = 2200;
const HISTORY_BYTES: usize = 1024 * 1024;
const HISTORY_REFRESH: Duration = Duration::from_secs(2);

struct HistoryReader {
    receiver: Receiver<String>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl HistoryReader {
    fn start(binding: &LiveBinding) -> io::Result<Self> {
        let socket = binding.namespace.session_socket_id.clone();
        let pane = binding.pane_id.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = Arc::clone(&stop);
        let (sender, receiver) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("peek-history".into())
            .spawn(move || {
                let mut last = None;
                while !cancelled.load(Ordering::Acquire) {
                    // ANSI reads are passive and do not scroll the source application.
                    let result = ProcessHerdr::default()
                        .with_socket(Some(socket.clone()))
                        .execute_bounded(
                            &[
                                "pane",
                                "read",
                                &pane,
                                "--source",
                                "recent-unwrapped",
                                "--lines",
                                "2200",
                                "--format",
                                "ansi",
                            ],
                            Duration::from_secs(20),
                            &cancelled,
                            ENCODED_LIMIT,
                        );
                    if let Ok(output) = result {
                        let clean = strip_terminal_sequences(&output.stdout);
                        let safe = snapshot::SafeText::from_untrusted(&clean, usize::MAX).text;
                        let text = newest_history(&safe);
                        if !text.is_empty()
                            && last.as_ref() != Some(&text)
                            && sender.try_send(text.clone()).is_ok()
                        {
                            last = Some(text);
                        }
                    }
                    if cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    thread::park_timeout(HISTORY_REFRESH);
                }
            })?;
        Ok(Self {
            receiver,
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for HistoryReader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

fn newest_history(text: &str) -> String {
    let skip = text.lines().count().saturating_sub(HISTORY_LINES);
    let start = if skip == 0 {
        0
    } else {
        text.match_indices('\n')
            .nth(skip - 1)
            .map_or(0, |(index, _)| index + 1)
    };
    let text = &text[start..];
    let mut bytes = 0;
    let mut encoded = 0;
    let mut start = text.len();
    for (index, ch) in text.char_indices().rev() {
        let char_bytes = ch.len_utf8();
        let json_bytes = match ch {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
            '\u{0}'..='\u{1f}' => 6,
            _ => char_bytes,
        };
        if bytes + char_bytes > HISTORY_BYTES || encoded + json_bytes + 64 > ENCODED_LIMIT {
            break;
        }
        bytes += char_bytes;
        encoded += json_bytes;
        start = index;
    }
    text[start..].to_owned()
}

#[derive(Default)]
pub(super) struct LivePeeks {
    active: AtomicUsize,
    inputs: Mutex<VecDeque<Instant>>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub(super) enum InputPost {
    #[serde(rename = "text")]
    Text { text: String, continuity: String },
    #[serde(rename = "key")]
    Key { key: InputKey, continuity: String },
}

#[derive(Deserialize)]
pub(super) enum InputKey {
    Enter,
    Escape,
    #[serde(rename = "Ctrl-C")]
    CtrlC,
    ArrowUp,
    ArrowDown,
}

impl InputKey {
    fn herdr(&self) -> &'static str {
        match self {
            Self::Enter => "enter",
            Self::Escape => "esc",
            Self::CtrlC => "ctrl+c",
            Self::ArrowUp => "up",
            Self::ArrowDown => "down",
        }
    }
}

impl InputPost {
    pub(super) fn read(reader: &mut impl Read, request: &Request) -> Result<Self, Status> {
        if request
            .header("content-type")
            .map(|value| value.split(';').next().unwrap_or("").trim())
            != Some("application/json")
        {
            return Err(Status::BadRequest);
        }
        let length = request.header("content-length").ok_or(Status::BadRequest)?;
        if length.is_empty() || !length.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(Status::BadRequest);
        }
        let length = length
            .parse::<usize>()
            .map_err(|_| Status::PayloadTooLarge)?;
        if length > INPUT_LIMIT {
            return Err(Status::PayloadTooLarge);
        }
        let mut bytes = vec![0; length];
        reader
            .read_exact(&mut bytes)
            .map_err(|_| Status::BadRequest)?;
        let post: Self = serde_json::from_slice(&bytes).map_err(|_| Status::BadRequest)?;
        let continuity = match &post {
            Self::Text { text, continuity } => {
                if text.is_empty()
                    || text.len() > TEXT_LIMIT
                    || text
                        .chars()
                        .any(|ch| ch.is_control() || (ch.is_whitespace() && ch != ' '))
                {
                    return Err(Status::BadRequest);
                }
                continuity
            }
            Self::Key { continuity, .. } => continuity,
        };
        if continuity.len() != 64
            || !continuity
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(Status::BadRequest);
        }
        Ok(post)
    }

    fn continuity(&self) -> &str {
        match self {
            Self::Text { continuity, .. } | Self::Key { continuity, .. } => continuity,
        }
    }
}
struct Permit<'a>(&'a AtomicUsize);
impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

pub(super) fn continuity_query(query: Option<&str>) -> Result<Option<String>, Status> {
    let Some(query) = query else {
        return Ok(None);
    };
    let Some(raw) = query
        .strip_prefix("continuity=")
        .filter(|raw| !raw.contains('&'))
    else {
        return Err(Status::BadRequest);
    };
    let value = decode_query_value(raw).ok_or(Status::BadRequest)?;
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(Status::BadRequest);
    }
    Ok(Some(value))
}

#[derive(Clone, Debug)]
struct NativeIdentity {
    terminal: String,
    session: Option<String>,
    status: Option<String>,
}
impl PartialEq for NativeIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.terminal == other.terminal && self.session == other.session
    }
}
impl Eq for NativeIdentity {}
impl NativeIdentity {
    fn accepts_input(&self) -> bool {
        !self
            .status
            .as_deref()
            .is_some_and(|status| matches!(status, "offline" | "disconnected" | "unknown"))
    }
}

struct NativePane {
    identity: NativeIdentity,
    source_rows: Option<serde_json::Value>,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct SourceViewport {
    cols: Option<u16>,
    rows: Option<u16>,
}
impl SourceViewport {
    fn bounded(cols: Option<u64>, rows: Option<u64>) -> Self {
        let cols = cols
            .filter(|cols| *cols > 0)
            .map(|cols| cols.min(512) as u16);
        let mut rows = rows
            .filter(|rows| *rows > 0)
            .map(|rows| rows.min(256) as u16);
        if let (Some(cols), Some(height)) = (cols, &mut rows) {
            *height = u32::from(*height).min(65536 / u32::from(cols)) as u16;
        }
        Self { cols, rows }
    }

    fn has_flags(self) -> bool {
        self.cols.is_some() || self.rows.is_some()
    }
}

fn tty_path(value: &str) -> Option<PathBuf> {
    if matches!(value, "" | "?" | "??" | "-" | "console" | "/dev/console")
        || value
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return None;
    }
    let path = Path::new(value);
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        Path::new("/dev").join(path)
    };
    let relative = path.strip_prefix("/dev").ok()?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
        || value.split('/').any(|part| matches!(part, "." | ".."))
    {
        return None;
    }
    Some(path)
}

fn tty_viewport(path: &Path) -> Option<SourceViewport> {
    let Ok(tty) = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK)
        .open(path)
    else {
        return None;
    };
    let fd = tty.as_raw_fd();
    // SAFETY: isatty and TIOCGWINSZ borrow this open descriptor without reading or changing the PTY.
    if unsafe { libc::isatty(fd) } != 1 {
        return None;
    }
    let mut size = std::mem::MaybeUninit::<libc::winsize>::uninit();
    // SAFETY: ioctl initializes the valid winsize allocation on success.
    if unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, size.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: TIOCGWINSZ succeeded above.
    let size = unsafe { size.assume_init() };
    if size.ws_col == 0 || size.ws_row == 0 {
        return None;
    }
    Some(SourceViewport::bounded(
        Some(u64::from(size.ws_col)),
        Some(u64::from(size.ws_row)),
    ))
}

fn shell_tty(pid: u64, budget: Duration) -> Option<PathBuf> {
    if pid == 0 || pid > i32::MAX as u64 || budget.is_zero() {
        return None;
    }
    let result = ProcessHerdr::for_binary("/bin/ps").execute_bounded(
        &["-p", &pid.to_string(), "-o", "tty="],
        budget.min(Duration::from_millis(150)),
        &AtomicBool::new(false),
        1024,
    );
    let Ok(result) = result else {
        return None;
    };
    tty_path(result.stdout.trim())
}

fn source_tty(binding: &LiveBinding, budget: Duration) -> Option<PathBuf> {
    if budget.is_zero() {
        return None;
    }
    let started = Instant::now();
    let result = ProcessHerdr::default()
        .with_socket(Some(binding.namespace.session_socket_id.clone()))
        .execute_bounded(
            &["pane", "process-info", "--pane", &binding.pane_id],
            budget.min(Duration::from_millis(150)),
            &AtomicBool::new(false),
            ENCODED_LIMIT,
        );
    let result = result.ok()?;
    let reply = serde_json::from_str::<serde_json::Value>(&result.stdout).ok()?;
    let info = reply.pointer("/result/process_info")?;
    if let Some(value) = info.get("tty").filter(|value| !value.is_null()) {
        tty_path(value.as_str()?)
    } else if let Some(value) = info.get("shell_pid").filter(|value| !value.is_null()) {
        shell_tty(value.as_u64()?, budget.saturating_sub(started.elapsed()))
    } else {
        None
    }
}

struct TtyDiscovery {
    path: Option<PathBuf>,
    next_probe: Instant,
}
impl TtyDiscovery {
    fn new() -> Self {
        Self {
            path: None,
            next_probe: Instant::now(),
        }
    }

    fn viewport(
        &mut self,
        pane: &NativePane,
        binding: &LiveBinding,
        budget: Duration,
    ) -> SourceViewport {
        if let Some(path) = &self.path {
            if let Some(viewport) = tty_viewport(path) {
                return viewport;
            }
            self.path = None;
        }
        if self.begin_probe(Instant::now(), budget)
            && let Some(path) = source_tty(binding, budget)
            && let Some(viewport) = tty_viewport(&path)
        {
            self.path = Some(path);
            return viewport;
        }
        SourceViewport::bounded(
            None,
            pane.source_rows
                .as_ref()
                .and_then(serde_json::Value::as_u64),
        )
    }

    fn begin_probe(&mut self, now: Instant, budget: Duration) -> bool {
        if now < self.next_probe || budget.is_zero() {
            return false;
        }
        self.next_probe = now + Duration::from_secs(1);
        true
    }
}

#[derive(Deserialize)]
struct PaneReply {
    result: PaneResult,
}
#[derive(Deserialize)]
struct PaneResult {
    panes: Vec<Pane>,
}
#[derive(Deserialize)]
struct Pane {
    pane_id: String,
    workspace_id: String,
    terminal_id: String,
    agent_session: Option<Session>,
    agent_status: Option<String>,
    scroll: Option<serde_json::Value>,
}
#[derive(Deserialize)]
struct Session {
    value: String,
}

fn recorded(store: &mut StateStore, key: &SpaceKey, address: &str) -> Result<LiveBinding, Status> {
    match store
        .dashboard_live_member(key, address)
        .map_err(|_| Status::Unavailable)?
    {
        LiveMember::Missing => Err(Status::NotFound),
        LiveMember::Gone => Err(Status::Gone),
        LiveMember::Bound { binding, .. } => Ok(binding),
    }
}

fn native(binding: &LiveBinding, budget: Duration) -> Result<NativePane, Status> {
    let result = ProcessHerdr::default()
        .with_socket(Some(binding.namespace.session_socket_id.clone()))
        .execute_bounded(
            &[
                "pane",
                "list",
                "--workspace",
                binding.namespace.space_id.as_str(),
            ],
            budget.min(Duration::from_millis(500)),
            &AtomicBool::new(false),
            ENCODED_LIMIT,
        )
        .map_err(|_| Status::Unavailable)?;
    let reply: PaneReply = serde_json::from_str(&result.stdout).map_err(|_| Status::Unavailable)?;
    let mut matching = reply.result.panes.into_iter().filter(|pane| {
        pane.pane_id == binding.pane_id && pane.workspace_id == binding.namespace.space_id.as_str()
    });
    let pane = matching.next().ok_or(Status::Gone)?;
    if matching.next().is_some()
        || pane.terminal_id.is_empty()
        || pane.terminal_id.len() > 512
        || pane.terminal_id.starts_with('-')
        || pane
            .terminal_id
            .chars()
            .any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(Status::Unavailable);
    }
    let identity = NativeIdentity {
        terminal: pane.terminal_id,
        session: pane
            .agent_session
            .map(|s| s.value)
            .filter(|s| !s.is_empty()),
        status: pane.agent_status,
    };
    if binding
        .agent_session_id
        .as_ref()
        .zip(identity.session.as_ref())
        .is_some_and(|(a, b)| a.as_str() != b)
    {
        return Err(Status::Gone);
    }
    Ok(NativePane {
        identity,
        source_rows: pane
            .scroll
            .and_then(|scroll| scroll.get("viewport_rows").cloned()),
    })
}

fn continuity(
    front: &Front,
    address: &str,
    binding: &LiveBinding,
    native: &NativeIdentity,
) -> String {
    let mut hash = Sha256::new();
    for value in [
        "namedrop-live/v1",
        front.instance.as_str(),
        binding.namespace.session_socket_id.as_str(),
        binding.namespace.space_id.as_str(),
        address,
        binding.binding_id.as_str(),
        binding.pane_id.as_str(),
        binding.pane_type.as_str(),
        native.terminal.as_str(),
    ] {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    }
    for session in [
        binding.agent_session_id.as_ref().map(|s| s.as_str()),
        native.session.as_deref(),
    ] {
        hash.update([u8::from(session.is_some())]);
        if let Some(value) = session {
            hash.update((value.len() as u64).to_be_bytes());
            hash.update(value.as_bytes());
        }
    }
    crate::lower_hex(&hash.finalize())
}

struct Observer {
    guard: Child,
    liveness: Option<ChildStdin>,
    stdout: ChildStdout,
}
impl Observer {
    fn open(
        binding: &LiveBinding,
        identity: &NativeIdentity,
        viewport: SourceViewport,
    ) -> io::Result<Self> {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg("dashboard-observer-guard")
            .arg(&identity.terminal);
        if let Some(rows) = viewport.rows {
            command.arg("--rows").arg(rows.to_string());
        }
        if let Some(cols) = viewport.cols {
            command.arg("--cols").arg(cols.to_string());
        }
        let mut guard = command
            .env("HERDR_SOCKET_PATH", &binding.namespace.session_socket_id)
            .env_remove("HERDR_PANE_ID")
            .env_remove("HERDR_WORKSPACE_ID")
            .env_remove("HERDR_TAB_ID")
            .env_remove("HERDR_PLUGIN_STATE_DIR")
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let liveness = guard.stdin.take();
        let stdout = guard
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("missing observer pipe"))?;
        let mut observer = Self {
            guard,
            liveness,
            stdout,
        };
        if let Err(error) = nonblocking(observer.stdout.as_raw_fd()) {
            observer.stop();
            return Err(error);
        }
        Ok(observer)
    }
    fn stop(&mut self) {
        // EOF reaches the guard even on dashboard SIGKILL. It kills/reaps its
        // separate observer group. We then reap the guard, without PTY signals.
        self.liveness.take();
        let _ = self.guard.wait();
    }
}
impl Drop for Observer {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Observation {
    observer: Observer,
    input: Input,
    normalizer: Normalizer,
    viewport: SourceViewport,
    tty: TtyDiscovery,
    flagless: bool,
    deadline: Option<Instant>,
}
impl Observation {
    fn open(binding: &LiveBinding, pane: &NativePane, deadline: Instant) -> Result<Self, Status> {
        let mut tty = TtyDiscovery::new();
        let viewport = tty.viewport(
            pane,
            binding,
            deadline.saturating_duration_since(Instant::now()),
        );
        if Instant::now() >= deadline {
            return Err(Status::Unavailable);
        }
        Ok(Self {
            observer: Observer::open(binding, &pane.identity, viewport)
                .map_err(|_| Status::Unavailable)?,
            input: Input::default(),
            normalizer: Normalizer::default(),
            viewport,
            tty,
            flagless: false,
            deadline: Some(deadline),
        })
    }

    fn reconcile(
        &mut self,
        binding: &LiveBinding,
        identity: &NativeIdentity,
        pane: NativePane,
    ) -> Result<bool, Status> {
        if pane.identity != *identity {
            return Err(Status::Gone);
        }
        let deadline = self.deadline.unwrap_or_else(|| Instant::now() + STARTUP);
        let viewport = self.tty.viewport(
            &pane,
            binding,
            deadline.saturating_duration_since(Instant::now()),
        );
        let viewport = SourceViewport::bounded(
            viewport.cols.or(self.viewport.cols).map(u64::from),
            viewport.rows.or(self.viewport.rows).map(u64::from),
        );
        if viewport == self.viewport {
            return Ok(false);
        }
        if Instant::now() >= deadline {
            return Err(Status::Unavailable);
        }
        self.observer.stop();
        self.observer =
            Observer::open(binding, &pane.identity, viewport).map_err(|_| Status::Unavailable)?;
        self.input = Input::default();
        self.normalizer = Normalizer::default();
        self.viewport = viewport;
        self.flagless = false;
        self.deadline = Some(deadline);
        Ok(true)
    }

    fn retry_flagless(
        &mut self,
        binding: &LiveBinding,
        identity: &NativeIdentity,
    ) -> io::Result<bool> {
        if !self.input.eof || self.input.saw_stdout || self.flagless || !self.viewport.has_flags() {
            return Ok(false);
        }
        // The guard owns stdout until exit, so EOF makes this wait finite.
        // Closing liveness first could mask usage exit 2.
        if self.observer.guard.wait()?.code() != Some(2) {
            return Ok(false);
        }
        self.observer.stop();
        self.observer = Observer::open(binding, identity, SourceViewport::default())?;
        self.input = Input::default();
        self.normalizer = Normalizer::default();
        self.flagless = true;
        Ok(true)
    }

    fn next(&mut self) -> Result<Option<NativeRecord>, ()> {
        self.input.next(&mut self.observer.stdout)
    }

    fn expired(&self) -> bool {
        self.deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
    }
}

fn nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: fcntl only reads/updates flags on this owned pipe/socket fd.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum NativeRecord {
    #[serde(rename = "terminal.frame")]
    Frame {
        bytes: String,
        encoding: String,
        full: bool,
        width: u16,
        height: u16,
        seq: u64,
    },
    #[serde(rename = "terminal.closed")]
    Closed {},
}
struct Paint {
    full: bool,
    cols: u16,
    rows: u16,
    bytes: Vec<u8>,
}
#[derive(Default)]
struct Normalizer {
    seq: u64,
    grid: Option<(u16, u16)>,
}
impl Normalizer {
    fn frame(&mut self, record: NativeRecord) -> Result<Option<Paint>, ()> {
        let NativeRecord::Frame {
            bytes,
            encoding,
            full,
            width,
            height,
            seq,
        } = record
        else {
            return Ok(None);
        };
        if encoding != "ansi"
            || Some(seq) != self.seq.checked_add(1)
            || width == 0
            || width > 512
            || height == 0
            || height > 256
            || u32::from(width) * u32::from(height) > 65536
            || (self.grid.is_none() && !full)
            || (!full && self.grid != Some((width, height)))
        {
            return Err(());
        }
        let bytes = decode(&bytes)?;
        self.seq = seq;
        self.grid = Some((width, height));
        Ok(Some(Paint {
            full,
            cols: width,
            rows: height,
            bytes,
        }))
    }
}

#[derive(Default)]
struct Input {
    buffer: Vec<u8>,
    scanned: usize,
    eof: bool,
    saw_stdout: bool,
}
impl Input {
    fn next(&mut self, pipe: &mut ChildStdout) -> Result<Option<NativeRecord>, ()> {
        if let Some(end) = self.buffer[self.scanned..]
            .iter()
            .position(|b| *b == b'\n')
            .map(|n| n + self.scanned)
        {
            if end > ENCODED_LIMIT {
                return Err(());
            }
            let record = serde_json::from_slice(&self.buffer[..end]).map_err(|_| ())?;
            self.buffer.drain(..=end);
            self.scanned = 0;
            return Ok(Some(record));
        }
        if self.buffer.len() > ENCODED_LIMIT {
            return Err(());
        }
        if self.eof {
            // A killed/disconnected source may leave a partial record. No
            // partial bytes are forwarded; reconnect starts from a full paint.
            self.buffer.clear();
            self.scanned = 0;
            return Ok(None);
        }
        self.scanned = self.buffer.len();
        if !self.eof {
            let mut bytes = [0; 16384];
            let budget = (ENCODED_LIMIT + 1 - self.buffer.len()).min(bytes.len());
            match pipe.read(&mut bytes[..budget]) {
                Ok(0) => self.eof = true,
                Ok(n) => {
                    self.saw_stdout = true;
                    self.buffer.extend_from_slice(&bytes[..n]);
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => return Err(()),
            }
        }
        Ok(None)
    }
}

#[derive(Default)]
struct Output {
    lines: VecDeque<(Vec<u8>, Option<u64>)>,
    offset: usize,
    pending: usize,
    since_progress: Option<Instant>,
    sent_seq: u64,
    headers_sent: bool,
}
impl Output {
    fn push(&mut self, bytes: Vec<u8>) -> Result<(), ()> {
        self.push_line(bytes, None)
    }
    fn push_line(&mut self, bytes: Vec<u8>, seq: Option<u64>) -> Result<(), ()> {
        if self.pending + bytes.len() > PENDING_LIMIT {
            return Err(());
        }
        if self.pending == 0 {
            self.since_progress = Some(Instant::now());
        }
        self.pending += bytes.len();
        self.lines.push_back((bytes, seq));
        Ok(())
    }
    fn paint(&mut self, paint: Paint, continuity: &str, seq: &mut u64) -> Result<(), ()> {
        if paint.full {
            let bytes = encode(&paint.bytes);
            self.record(
                seq,
                Event::Reset {
                    continuity,
                    cols: paint.cols,
                    rows: paint.rows,
                    bytes: &bytes,
                },
            )
        } else {
            for chunk in paint.bytes.chunks(DATA_LIMIT) {
                let bytes = encode(chunk);
                self.record(seq, Event::Data { bytes: &bytes })?;
            }
            Ok(())
        }
    }
    fn record(&mut self, seq: &mut u64, event: Event<'_>) -> Result<(), ()> {
        *seq = seq
            .checked_add(1)
            .filter(|n| *n <= 9_007_199_254_740_991)
            .ok_or(())?;
        self.push_line(Record::line(*seq, event)?, Some(*seq))
    }
    fn discard_and_end(&mut self, seq: &mut u64, event: Event<'_>) -> Result<(), ()> {
        if !self.headers_sent || self.offset != 0 {
            return Err(());
        }
        self.lines.clear();
        self.pending = 0;
        self.since_progress = None;
        *seq = self.sent_seq;
        self.record(seq, event)
    }
    fn write(&mut self, socket: &mut TcpStream) -> io::Result<()> {
        if let Some((line, seq)) = self.lines.front() {
            match socket.write(&line[self.offset..]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => {
                    self.offset += n;
                    self.pending -= n;
                    self.since_progress = Some(Instant::now());
                    if self.offset == line.len() {
                        if let Some(seq) = seq {
                            self.sent_seq = *seq;
                        }
                        if line.starts_with(b"HTTP/") {
                            self.headers_sent = true;
                        }
                        self.lines.pop_front();
                        self.offset = 0;
                    }
                    if self.pending == 0 {
                        self.since_progress = None;
                    }
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e),
            }
        }
        if self.since_progress.is_some_and(|at| at.elapsed() >= STALL) {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(())
    }
}

fn disconnected(socket: &TcpStream) -> bool {
    match socket.peek(&mut [0]) {
        Ok(_) => true, // EOF or unexpected input after the read-only request.
        Err(e) => !matches!(
            e.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
        ),
    }
}

fn pause(socket: &TcpStream, pipe: RawFd, writing: bool) -> io::Result<()> {
    let mut fds = [
        libc::pollfd {
            fd: socket.as_raw_fd(),
            events: libc::POLLIN | if writing { libc::POLLOUT } else { 0 },
            revents: 0,
        },
        libc::pollfd {
            fd: pipe,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    // SAFETY: poll borrows the two valid descriptors owned by this handler.
    if unsafe { libc::poll(fds.as_mut_ptr(), 2, 25) } < 0
        && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

impl LivePeeks {
    pub(super) fn send_input(
        &self,
        front: &Front,
        key: &SpaceKey,
        address: &str,
        post: InputPost,
    ) -> Result<(), Status> {
        let _admitted = front.admit_mutation()?;
        {
            let mut recent = self.inputs.lock().map_err(|_| Status::Unavailable)?;
            let now = Instant::now();
            while recent
                .front()
                .is_some_and(|at| now.duration_since(*at) >= Duration::from_secs(1))
            {
                recent.pop_front();
            }
            if recent.len() >= INPUT_RATE {
                return Err(Status::TooManyRequests);
            }
            recent.push_back(now);
        }
        let mut store = StateStore::open_read_only(front.files.state())
            .map_err(|_| Status::Unavailable)?
            .ok_or(Status::NotFound)?;
        store
            .set_busy_timeout(Duration::from_millis(100))
            .map_err(|_| Status::Unavailable)?;
        let binding = recorded(&mut store, key, address)?;
        if binding.pane_type != crate::PaneType::Agent || binding.agent_session_id.is_none() {
            return Err(Status::Gone);
        }
        let identity = native(&binding, Duration::from_millis(500))?.identity;
        if !identity.accepts_input() {
            return Err(Status::Gone);
        }
        if identity.session.as_deref() != binding.agent_session_id.as_ref().map(|id| id.as_str()) {
            return Err(Status::Gone);
        }
        if post.continuity() != continuity(front, address, &binding, &identity) {
            return Err(Status::Gone);
        }
        let herdr =
            ProcessHerdr::default().with_socket(Some(binding.namespace.session_socket_id.clone()));
        let send = |args: &[&str]| {
            herdr
                .execute_bounded(args, Duration::from_secs(2), &AtomicBool::new(false), 4096)
                .map(|_| ())
                .map_err(|_| Status::Unavailable)
        };
        match post {
            InputPost::Text { text, .. } => {
                send(&["pane", "send-text", &binding.pane_id, &text])?;
                let current = recorded(&mut store, key, address)?;
                let updated = native(&binding, Duration::from_millis(500))?.identity;
                if current != binding || updated != identity || !updated.accepts_input() {
                    return Err(Status::Gone);
                }
                send(&["pane", "send-keys", &binding.pane_id, "enter"])
            }
            InputPost::Key { key, .. } => {
                send(&["pane", "send-keys", &binding.pane_id, key.herdr()])
            }
        }
    }

    pub(super) fn serve(
        &self,
        mut socket: TcpStream,
        front: &Front,
        key: &SpaceKey,
        address: &str,
        expected: Option<&str>,
    ) -> io::Result<()> {
        let started = Instant::now();
        socket.set_write_timeout(Some(Duration::from_secs(1)))?;
        let Ok(_) = self
            .active
            .try_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                (n < 8).then_some(n + 1)
            })
        else {
            return respond_status(&mut socket, Status::Unavailable);
        };
        let _permit = Permit(&self.active);
        let setup = (|| {
            if front.restarting.load(Ordering::Acquire) || front.files.restarting() {
                return Err(Status::Unavailable);
            }
            let mut store = StateStore::open_read_only(front.files.state())
                .map_err(|_| Status::Unavailable)?
                .ok_or(Status::NotFound)?;
            store
                .set_busy_timeout(Duration::from_millis(100))
                .map_err(|_| Status::Unavailable)?;
            let binding = recorded(&mut store, key, address)?;
            let pane = native(&binding, STARTUP.saturating_sub(started.elapsed()))?;
            let continuity = continuity(front, address, &binding, &pane.identity);
            if expected.is_some_and(|value| value != continuity) {
                return Err(Status::Gone);
            }
            Ok((store, binding, pane, continuity))
        })();
        let (mut store, binding, pane, continuity) = match setup {
            Ok(value) => value,
            Err(status) => return respond_status(&mut socket, status),
        };
        let mut observation = match Observation::open(&binding, &pane, started + STARTUP) {
            Ok(observation) => observation,
            Err(status) => return respond_status(&mut socket, status),
        };
        let identity = pane.identity;
        socket.set_nonblocking(true)?;
        let mut seq = 0;
        let mut output = Output::default();
        // Validate the entire first full paint before committing HTTP headers.
        let first = loop {
            let first = loop {
                if started.elapsed() >= STARTUP
                    || front.restarting.load(Ordering::Acquire)
                    || front.files.restarting()
                {
                    socket.set_nonblocking(false)?;
                    return respond_status(&mut socket, Status::Unavailable);
                }
                if disconnected(&socket) {
                    return Ok(());
                }
                match observation.next() {
                    Ok(Some(record)) => match observation.normalizer.frame(record) {
                        Ok(Some(paint)) => break Ok(paint),
                        Ok(None) => {
                            break Err(Event::Closed {
                                reason: Closed::Ended,
                            });
                        }
                        Err(()) => {
                            break Err(Event::Unavailable {
                                reason: Unavailable::InvalidSource,
                            });
                        }
                    },
                    Err(()) => {
                        break Err(Event::Unavailable {
                            reason: Unavailable::InvalidSource,
                        });
                    }
                    Ok(None) if observation.input.eof => {
                        if observation.retry_flagless(&binding, &identity)? {
                            continue;
                        }
                        // Only the exact CLI usage exit, with no output at all,
                        // proves this fixed valid invocation is unsupported.
                        observation.observer.stop();
                        let unsupported = observation.observer.guard.wait()?.code() == Some(2)
                            && !observation.input.saw_stdout;
                        if unsupported {
                            break Err(Event::Unavailable {
                                reason: Unavailable::Unsupported,
                            });
                        }
                        socket.set_nonblocking(false)?;
                        return respond_status(&mut socket, Status::Unavailable);
                    }
                    Ok(None) => {}
                }
                pause(&socket, observation.observer.stdout.as_raw_fd(), false)?;
            };
            let rechecked = recorded(&mut store, key, address).and_then(|current| {
                if current != binding {
                    return Err(Status::Gone);
                }
                let current = native(&binding, STARTUP.saturating_sub(started.elapsed()))?;
                observation.reconcile(&binding, &identity, current)
            });
            match rechecked {
                Ok(true) => continue,
                Ok(false) => {}
                Err(status) => {
                    socket.set_nonblocking(false)?;
                    return respond_status(&mut socket, status);
                }
            }
            if started.elapsed() >= STARTUP {
                socket.set_nonblocking(false)?;
                return respond_status(&mut socket, Status::Unavailable);
            }
            break first;
        };
        output.push(format!("HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\n{SECURITY_HEADERS}Connection: close\r\n\r\n").into_bytes()).map_err(|_|io::Error::other("live output limit"))?;
        let mut ending = match first {
            Ok(paint) => {
                output
                    .paint(paint, &continuity, &mut seq)
                    .map_err(|_| io::Error::other("live output limit"))?;
                observation.deadline = None;
                false
            }
            Err(event) => {
                output
                    .record(&mut seq, event)
                    .map_err(|_| io::Error::other("live output limit"))?;
                true
            }
        };
        let history = (!ending)
            .then(|| HistoryReader::start(&binding).ok())
            .flatten();
        let mut native_checked = Instant::now();
        let mut last_heartbeat = Instant::now();
        loop {
            if disconnected(&socket) {
                return Ok(());
            }
            if !ending {
                let mut final_event = match recorded(&mut store, key, address) {
                    Ok(current) if current == binding => None,
                    Err(Status::NotFound) => Some(Event::Closed {
                        reason: Closed::Missing,
                    }),
                    Err(Status::Unavailable) => Some(Event::Unavailable {
                        reason: Unavailable::Busy,
                    }),
                    _ => Some(Event::Closed {
                        reason: Closed::Replaced,
                    }),
                };
                let history_text = history
                    .as_ref()
                    .and_then(|history| history.receiver.try_recv().ok());
                if final_event.is_none()
                    && (native_checked.elapsed() >= Duration::from_secs(1)
                        || history_text.is_some())
                {
                    native_checked = Instant::now();
                    match native(&binding, Duration::from_millis(500))
                        .and_then(|current| observation.reconcile(&binding, &identity, current))
                    {
                        Ok(_) => {}
                        Err(Status::Unavailable) => {
                            final_event = Some(Event::Unavailable {
                                reason: Unavailable::SourceLost,
                            });
                        }
                        _ => {
                            final_event = Some(Event::Closed {
                                reason: Closed::Replaced,
                            });
                        }
                    }
                }
                if front.restarting.load(Ordering::Acquire) || front.files.restarting() {
                    final_event = Some(Event::Unavailable {
                        reason: Unavailable::ServerStopping,
                    });
                }
                if final_event.is_none() && observation.expired() {
                    final_event = Some(Event::Unavailable {
                        reason: Unavailable::SourceLost,
                    });
                }
                if final_event.is_none()
                    && let Some(text) = history_text
                {
                    let line = Record::line(seq + 1, Event::History { text: &text });
                    if line.is_ok_and(|line| output.pending + line.len() <= PENDING_LIMIT) {
                        output
                            .record(&mut seq, Event::History { text: &text })
                            .map_err(|_| io::Error::other("live history output limit"))?;
                    }
                }
                if let Some(event) = final_event {
                    observation.observer.stop();
                    // End at the last complete record. A partially written line
                    // can only end in EOF; the continuity retry then refuses it.
                    if output.discard_and_end(&mut seq, event).is_err() {
                        return Ok(());
                    }
                    ending = true;
                } else {
                    let event = match observation.next() {
                        Ok(Some(record)) => match observation.normalizer.frame(record) {
                            Ok(Some(paint)) => {
                                if output.paint(paint, &continuity, &mut seq).is_err() {
                                    observation.observer.stop();
                                    if output
                                        .discard_and_end(
                                            &mut seq,
                                            Event::Unavailable {
                                                reason: Unavailable::SlowViewer,
                                            },
                                        )
                                        .is_err()
                                    {
                                        return Ok(());
                                    }
                                    ending = true;
                                } else {
                                    observation.deadline = None;
                                }
                                None
                            }
                            Ok(None) => Some(Event::Closed {
                                reason: Closed::Ended,
                            }),
                            Err(()) => Some(Event::Unavailable {
                                reason: Unavailable::InvalidSource,
                            }),
                        },
                        Err(()) => Some(Event::Unavailable {
                            reason: Unavailable::InvalidSource,
                        }),
                        Ok(None) if observation.input.eof => {
                            if observation.retry_flagless(&binding, &identity)? {
                                None
                            } else {
                                Some(Event::Unavailable {
                                    reason: Unavailable::SourceLost,
                                })
                            }
                        }
                        Ok(None) => None,
                    };
                    if let Some(event) = event {
                        observation.observer.stop();
                        if output.record(&mut seq, event).is_err() {
                            return Ok(());
                        }
                        ending = true;
                    }
                }
            }
            if !ending && output.pending == 0 && last_heartbeat.elapsed() >= HEARTBEAT {
                output
                    .push(vec![b'\n'])
                    .map_err(|_| io::Error::other("live output limit"))?;
                last_heartbeat = Instant::now();
            }
            if output.write(&mut socket).is_err() {
                return Ok(());
            }
            if ending && output.pending == 0 {
                return Ok(());
            }
            pause(
                &socket,
                if ending {
                    -1
                } else {
                    observation.observer.stdout.as_raw_fd()
                },
                output.pending > 0,
            )?;
        }
    }
}

#[derive(Serialize)]
struct Record<'a> {
    v: u8,
    seq: u64,
    #[serde(flatten)]
    event: Event<'a>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Event<'a> {
    Reset {
        continuity: &'a str,
        cols: u16,
        rows: u16,
        bytes: &'a str,
    },
    Data {
        bytes: &'a str,
    },
    History {
        text: &'a str,
    },
    Closed {
        reason: Closed,
    },
    Unavailable {
        reason: Unavailable,
    },
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Closed {
    Ended,
    Replaced,
    Missing,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Unavailable {
    Unsupported,
    Busy,
    SourceLost,
    InvalidSource,
    SlowViewer,
    ServerStopping,
}

impl Record<'_> {
    fn line(seq: u64, event: Event<'_>) -> Result<Vec<u8>, ()> {
        let mut line = serde_json::to_vec(&Record { v: 1, seq, event }).map_err(|_| ())?;
        if line.len() > ENCODED_LIMIT {
            return Err(());
        }
        line.push(b'\n');
        Ok(line)
    }
}

fn encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        output.push(DIGITS[usize::from(a >> 2)] as char);
        output.push(DIGITS[usize::from((a & 3) << 4 | b >> 4)] as char);
        output.push(if chunk.len() > 1 {
            DIGITS[usize::from((b & 15) << 2 | c >> 6)] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            DIGITS[usize::from(c & 63)] as char
        } else {
            '='
        });
    }
    output
}

fn decode(text: &str) -> Result<Vec<u8>, ()> {
    fn digit(value: u8) -> Result<u8, ()> {
        match value {
            b'A'..=b'Z' => Ok(value - b'A'),
            b'a'..=b'z' => Ok(value - b'a' + 26),
            b'0'..=b'9' => Ok(value - b'0' + 52),
            b'+' => Ok(62),
            b'/' => Ok(63),
            _ => Err(()),
        }
    }
    if !text.len().is_multiple_of(4) || text.len() > PAINT_LIMIT.div_ceil(3) * 4 {
        return Err(());
    }
    let mut output = Vec::with_capacity((text.len() / 4 * 3).min(PAINT_LIMIT));
    let count = text.len() / 4;
    for (i, part) in text.as_bytes().as_chunks::<4>().0.iter().enumerate() {
        let a = digit(part[0])?;
        let b = digit(part[1])?;
        output.push(a << 2 | b >> 4);
        if part[2] == b'=' {
            if part[3] != b'=' || i + 1 != count || b & 15 != 0 {
                return Err(());
            }
        } else {
            let c = digit(part[2])?;
            output.push(b << 4 | c >> 2);
            if part[3] == b'=' {
                if i + 1 != count || c & 3 != 0 {
                    return Err(());
                }
            } else {
                output.push(c << 6 | digit(part[3])?);
            }
        }
        if output.len() > PAINT_LIMIT {
            return Err(());
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn tty_discovery_reserves_one_second_between_probe_starts() {
        let mut discovery = TtyDiscovery::new();
        let now = discovery.next_probe;
        let budget = Duration::from_millis(150);
        assert!(!discovery.begin_probe(now, Duration::ZERO));
        assert!(discovery.begin_probe(now, budget));
        assert!(!discovery.begin_probe(now, budget));
        assert!(!discovery.begin_probe(now + Duration::from_millis(999), budget));
        assert!(!discovery.begin_probe(now + Duration::from_secs(1), Duration::ZERO));
        assert!(discovery.begin_probe(now + Duration::from_secs(1), budget));
        assert!(!discovery.begin_probe(now + Duration::from_millis(1999), budget));
        assert!(discovery.begin_probe(now + Duration::from_secs(2), budget));
    }

    #[test]
    fn geometry_hints_clamp_positive_axes_and_ignore_zero() {
        for (cols, rows, expected) in [
            (
                Some(1),
                Some(256),
                SourceViewport {
                    cols: Some(1),
                    rows: Some(256),
                },
            ),
            (
                Some(600),
                Some(300),
                SourceViewport {
                    cols: Some(512),
                    rows: Some(128),
                },
            ),
            (
                Some(200),
                Some(300),
                SourceViewport {
                    cols: Some(200),
                    rows: Some(256),
                },
            ),
            (Some(0), Some(0), SourceViewport::default()),
            (
                None,
                Some(u64::MAX),
                SourceViewport {
                    cols: None,
                    rows: Some(256),
                },
            ),
        ] {
            assert_eq!(SourceViewport::bounded(cols, rows), expected);
        }
    }

    #[test]
    fn tty_names_accept_linux_and_macos_devices_and_refuse_parent_paths() {
        for (name, expected) in [
            ("pts/12", "/dev/pts/12"),
            ("/dev/pts/12", "/dev/pts/12"),
            ("ttys012", "/dev/ttys012"),
            ("/dev/ttys012", "/dev/ttys012"),
        ] {
            assert_eq!(tty_path(name).unwrap(), Path::new(expected));
        }
        for name in [
            "",
            "?",
            "??",
            "-",
            "console",
            "/dev/console",
            "/dev",
            "/tmp/tty",
            "../tty",
            "pts/../12",
            "/dev/./pts/12",
            "ttys012\n",
            "/dev/pts/12 extra",
        ] {
            assert!(tty_path(name).is_none(), "accepted {name:?}");
        }
    }

    #[test]
    fn normalization_checks_full_sequence_grid_and_decoded_limits() {
        let frame = |seq, full, width, height, bytes| NativeRecord::Frame {
            seq,
            full,
            width,
            height,
            bytes,
            encoding: "ansi".into(),
        };
        let mut normalizer = Normalizer::default();
        assert!(
            normalizer
                .frame(frame(1, false, 120, 40, encode(b"x")))
                .is_err()
        );
        let initial = normalizer
            .frame(frame(1, true, 120, 40, encode(b"x")))
            .unwrap()
            .unwrap();
        assert!(initial.full);
        assert!(
            normalizer
                .frame(frame(3, false, 120, 40, encode(b"gap")))
                .is_err()
        );
        assert!(
            normalizer
                .frame(frame(2, false, 100, 40, encode(b"size")))
                .is_err()
        );
        let replacement = normalizer
            .frame(frame(2, true, 120, 40, encode(b"replacement")))
            .unwrap()
            .unwrap();
        assert!(replacement.full);
        for (width, height) in [(0, 40), (513, 40), (256, 257), (512, 256)] {
            assert!(
                Normalizer::default()
                    .frame(frame(1, true, width, height, String::new()))
                    .is_err()
            );
        }
        assert!(
            Normalizer::default()
                .frame(frame(1, true, 512, 128, encode(&vec![0; PAINT_LIMIT])))
                .is_ok()
        );
        assert!(
            Normalizer::default()
                .frame(frame(1, true, 1, 1, encode(&vec![0; PAINT_LIMIT + 1])))
                .is_err()
        );
    }

    #[test]
    fn large_delta_splits_without_dropping_or_reordering_and_output_is_bounded() {
        let bytes: Vec<_> = (0..PAINT_LIMIT).map(|i| (i % 251) as u8).collect();
        let mut output = Output::default();
        let mut seq = 0;
        output
            .paint(
                Paint {
                    full: false,
                    cols: 120,
                    rows: 40,
                    bytes: bytes.clone(),
                },
                "c",
                &mut seq,
            )
            .unwrap();
        let mut joined = Vec::new();
        for (i, (line, _)) in output.lines.iter().enumerate() {
            let record: Value = serde_json::from_slice(line).unwrap();
            assert_eq!(record["seq"], i + 1);
            let chunk = decode(record["bytes"].as_str().unwrap()).unwrap();
            assert!(chunk.len() <= DATA_LIMIT);
            joined.extend(chunk);
        }
        assert_eq!(joined, bytes);
        assert_eq!(seq, 16);
        assert!(
            output
                .paint(
                    Paint {
                        full: true,
                        cols: 120,
                        rows: 40,
                        bytes
                    },
                    "c",
                    &mut seq
                )
                .is_err()
        );
        assert!(output.pending <= PENDING_LIMIT);
        assert!(
            Record::line(
                1,
                Event::Data {
                    bytes: &"x".repeat(ENCODED_LIMIT)
                }
            )
            .is_err()
        );
    }

    #[test]
    fn query_and_discarded_output_do_not_broaden_identity_or_create_sequence_gaps() {
        for query in ["", "other=x", "continuity=a&continuity=b", "continuity=ABC"] {
            assert!(continuity_query(Some(query)).is_err());
        }
        assert!(continuity_query(None).unwrap().is_none());
        assert_eq!(
            continuity_query(Some(&format!("continuity={}", "a".repeat(64)))).unwrap(),
            Some("a".repeat(64))
        );
        let mut output = Output {
            headers_sent: true,
            sent_seq: 3,
            ..Output::default()
        };
        let mut seq = 3;
        output
            .record(&mut seq, Event::Data { bytes: "eA==" })
            .unwrap();
        output
            .discard_and_end(
                &mut seq,
                Event::Closed {
                    reason: Closed::Replaced,
                },
            )
            .unwrap();
        assert_eq!(seq, 4);
        assert_eq!(output.lines.len(), 1);
        let record: Value = serde_json::from_slice(&output.lines[0].0).unwrap();
        assert_eq!(record["seq"], 4);
        assert_eq!(record["kind"], "closed");
        output.offset = 1;
        assert!(
            output
                .discard_and_end(
                    &mut seq,
                    Event::Closed {
                        reason: Closed::Replaced
                    }
                )
                .is_err()
        );
    }

    #[test]
    fn base64_is_canonical_and_bounded() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(encode(plain.as_bytes()), encoded);
            assert_eq!(decode(encoded).unwrap(), plain.as_bytes());
        }
        for invalid in ["Zg", "Zh==", "Zm9=", "Zg==Zg==", "====", "Zm-v", "Zg==\n"] {
            assert!(decode(invalid).is_err(), "{invalid}");
        }
        let bytes: Vec<u8> = (0..=255).collect();
        assert_eq!(decode(&encode(&bytes)).unwrap(), bytes);
        assert_eq!(
            decode(&encode(&vec![0; PAINT_LIMIT])).unwrap().len(),
            PAINT_LIMIT
        );
        assert!(decode(&encode(&vec![0; PAINT_LIMIT + 1])).is_err());
    }

    #[test]
    fn history_keeps_newest_lines_within_text_and_record_limits() {
        let text = (0..2300)
            .map(|i| format!("line-{i:04}\n"))
            .collect::<String>();
        let newest = newest_history(&text);
        assert!(newest.starts_with("line-0100\n"));
        assert_eq!(newest.lines().count(), HISTORY_LINES);
        let newest = newest_history(text.trim_end_matches('\n'));
        assert!(newest.starts_with("line-0100\n"));
        assert_eq!(newest.lines().count(), HISTORY_LINES);

        let escaped = "\"".repeat(HISTORY_BYTES);
        let newest = newest_history(&escaped);
        assert!(newest.len() <= HISTORY_BYTES);
        assert!(Record::line(1, Event::History { text: &newest }).is_ok());
        assert!(newest.len() < escaped.len());
        assert!(newest.len() > escaped.len() - 100);
    }

    #[test]
    fn authoritative_browser_fixture_is_actual_serialization() {
        fn value(seq: u64, event: Event<'_>) -> Value {
            serde_json::from_slice(&Record::line(seq, event).unwrap()).unwrap()
        }
        let mut streams = vec![json!({"name":"reset-data-reset-ended","records":[
            value(1,Event::Reset{continuity:"fixture-continuity",cols:120,rows:40,bytes:"G1syShtbSEhlbGxv"}),
            value(2,Event::Data{bytes:"G1szMW1SZWQ="}),
            value(3,Event::Reset{continuity:"fixture-continuity",cols:40,rows:10,bytes:"G1tITmV3"}),
            value(4,Event::Closed{reason:Closed::Ended})]})];
        streams.push(json!({"name":"reset-history-data-ended","records":[
            value(1,Event::Reset{continuity:"fixture-continuity",cols:120,rows:40,bytes:"G1syShtbSEhlbGxv"}),
            value(2,Event::History{text:"recent line one\nrecent line two\n"}),
            value(3,Event::Data{bytes:"G1szMW1SZWQ="}),
            value(4,Event::Closed{reason:Closed::Ended})]}));
        for reason in [Closed::Replaced, Closed::Missing] {
            let name = match reason {
                Closed::Ended => "ended",
                Closed::Replaced => "replaced",
                Closed::Missing => "missing",
            };
            streams.push(json!({"name":name,"records":[value(1,Event::Closed{reason})]}));
        }
        for reason in [
            Unavailable::Unsupported,
            Unavailable::Busy,
            Unavailable::SourceLost,
            Unavailable::InvalidSource,
            Unavailable::SlowViewer,
            Unavailable::ServerStopping,
        ] {
            let name = match reason {
                Unavailable::Unsupported => "unsupported",
                Unavailable::Busy => "busy",
                Unavailable::SourceLost => "source_lost",
                Unavailable::InvalidSource => "invalid_source",
                Unavailable::SlowViewer => "slow_viewer",
                Unavailable::ServerStopping => "server_stopping",
            };
            streams.push(json!({"name":name,"records":[value(1,Event::Unavailable{reason})]}));
        }
        let fixture = json!({"version":1,"limits":{"encoded_record":ENCODED_LIMIT,"decoded_paint":PAINT_LIMIT,"decoded_data":DATA_LIMIT,"pending_http":PENDING_LIMIT,"cols":512,"rows":256,"cells":65536},"streams":streams});
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/live_peek/streams.json"
        );
        if std::env::var_os("NAMEDROP_UPDATE_LIVE_FIXTURE").is_some() {
            std::fs::create_dir_all(std::path::Path::new(path).parent().unwrap()).unwrap();
            std::fs::write(
                path,
                format!("{}\n", serde_json::to_string_pretty(&fixture).unwrap()),
            )
            .unwrap();
        }
        assert_eq!(
            serde_json::from_str::<Value>(&std::fs::read_to_string(path).unwrap()).unwrap(),
            fixture
        );
    }
}
