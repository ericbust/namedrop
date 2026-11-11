use super::hub::{Frame, HubCommand, SpaceEvent, Subscription};
use super::server_files::{ServerFiles, Token, UiSource, open_directory};
use super::snapshot::{self, SpaceKey};
use crate::{HerdrCommandExecutor, ProcessHerdr, StateStore, store::dashboard::ScreenPane};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    ffi::CString,
    fs::File,
    io::{self, BufRead, BufReader, Read, Write},
    net::{Shutdown, TcpListener, TcpStream},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, RecvTimeoutError, Sender},
    },
    thread,
    time::{Duration, Instant},
};

#[path = "channels_bundle.rs"]
mod channels_bundle;
#[path = "live_peek.rs"]
mod live_peek;
#[path = "skills_api.rs"]
mod skills_api;
/// Caps an operator post and the full body Show more reads, so the page can
/// always show at least what it could send.
const BODY_LIMIT: usize = 64 * 1024;
/// The full-body route redacts before it cuts, so it reads this many
/// characters past the cut to see whole any secret that spans it.
const BODY_READ_MARGIN: usize = 4 * 1024;
const HEADER_LIMIT: u64 = 8 * 1024;
const PING_EVERY: Duration = Duration::from_secs(15);
const MAX_CONNECTIONS: usize = 64;
// Followers occupy HTTP handlers too. Keep 48 connection slots for other routes.
const MAX_SCREEN_REQUESTS: usize = 16;
macro_rules! protection_headers {
    () => {
        "Content-Security-Policy: default-src 'none'; script-src 'self'; \
style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; \
base-uri 'none'; form-action 'none'\r\nX-Frame-Options: DENY\r\n\
X-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\n"
    };
}
const SECURITY_HEADERS: &str = concat!(protection_headers!(), "Cache-Control: no-store\r\n");
/// An image's name is the digest of its bytes, so the browser may keep it
/// for as long as the server does.
const IMAGE_HEADERS: &str = concat!(
    protection_headers!(),
    "Cache-Control: private, max-age=2592000, immutable\r\n"
);
/// The composer sends a message's images together.
const MAX_IMAGE_UPLOADS: usize = super::images::MAX_PER_MESSAGE;

pub(crate) struct Front {
    pub port: u16,
    pub files: ServerFiles,
    pub token: Token,
    pub session: Token,
    pub instance: String,
    pub hub: Sender<HubCommand>,
    pub assets: Assets,
    pub restarting: Arc<AtomicBool>,
    pub mutations: Arc<Mutex<()>>,
    pub channel_wake: super::channel_service::ChannelWake,
    pub control_effects: super::thread_controls::ControlEffects,
    pub image_uploads: AtomicUsize,
}

impl Front {
    fn admit_mutation(&self) -> Result<std::sync::MutexGuard<'_, ()>, Status> {
        let guard = self.mutations.lock().map_err(|_| Status::Unavailable)?;
        if self.restarting.load(Ordering::Acquire) || self.files.restarting() {
            return Err(Status::Unavailable);
        }
        Ok(guard)
    }
}

pub(crate) fn accept_loop(listener: TcpListener, front: Arc<Front>, stop: Arc<AtomicBool>) {
    let next_id = Arc::new(AtomicU64::new(1));
    let open = Arc::new(AtomicUsize::new(0));
    let history_store = Arc::new(Mutex::new(HistoryStore::Unopened));
    let screens = Arc::new(ScreenReads::default());
    let live = Arc::new(live_peek::LivePeeks::default());
    let skills = Arc::new(super::skills::SkillCatalog::default());
    for mut stream in listener.incoming().flatten() {
        if stop.load(Ordering::Acquire) {
            break;
        }
        if open.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
            open.fetch_sub(1, Ordering::AcqRel);
            let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
            let _ = respond(
                &mut stream,
                "503 Service Unavailable",
                "text/plain; charset=utf-8",
                "",
                b"Too many connections.\n",
            );
            continue;
        }
        let permit = ConnectionPermit(open.clone());
        let front = front.clone();
        let next_id = next_id.clone();
        let history_store = history_store.clone();
        let screens = screens.clone();
        let live = live.clone();
        let skills = skills.clone();
        thread::spawn(move || {
            let _permit = permit;
            let _ = serve_connection(
                stream,
                &front,
                &next_id,
                &history_store,
                &screens,
                &live,
                &skills,
            );
        });
    }
}

enum HistoryStore {
    Unopened,
    Ready(StateStore),
    Failed,
}

impl HistoryStore {
    fn store(&mut self, state: &Path) -> Result<&mut StateStore, Status> {
        if matches!(self, Self::Unopened) {
            let opened = StateStore::open_read_only(state).and_then(|store| {
                if let Some(store) = &store {
                    store.set_busy_timeout(Duration::from_millis(250))?;
                }
                Ok(store)
            });
            *self = match opened {
                Ok(Some(store)) => Self::Ready(store),
                Ok(None) => Self::Unopened,
                Err(_) => Self::Failed,
            };
        }
        match self {
            Self::Ready(store) => Ok(store),
            Self::Unopened => Err(Status::NotFound),
            Self::Failed => Err(Status::Unavailable),
        }
    }
}

#[derive(Clone, Eq, PartialEq, Hash)]
struct ScreenTarget {
    socket: String,
    pane: String,
}

type ScreenResult = Result<Arc<str>, Status>;

#[derive(Default)]
struct ScreenReads {
    active: AtomicUsize,
    flights: Mutex<HashMap<ScreenTarget, Arc<ScreenFlight>>>,
}

#[derive(Default)]
struct ScreenFlight {
    result: Mutex<Option<ScreenResult>>,
    ready: Condvar,
}

struct ScreenPermit<'a>(&'a AtomicUsize);

impl Drop for ScreenPermit<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl ScreenReads {
    fn try_admit(&self) -> Result<ScreenPermit<'_>, Status> {
        self.active
            .try_update(Ordering::AcqRel, Ordering::Acquire, |active| {
                (active < MAX_SCREEN_REQUESTS).then_some(active + 1)
            })
            .map(|_| ScreenPermit(&self.active))
            .map_err(|_| Status::Unavailable)
    }

    fn read(&self, target: ScreenTarget) -> ScreenResult {
        let (flight, leader) = {
            let mut flights = self.flights.lock().map_err(|_| Status::Unavailable)?;
            match flights.get(&target) {
                Some(flight) => (flight.clone(), false),
                None => {
                    let flight = Arc::new(ScreenFlight::default());
                    flights.insert(target.clone(), flight.clone());
                    (flight, true)
                }
            }
        };
        if leader {
            let mut completion = ScreenCompletion {
                reads: self,
                target: &target,
                flight: &flight,
                pending: true,
            };
            let result = read_screen(&target);
            completion.publish(result.clone());
            result
        } else {
            let result = flight.result.lock().map_err(|_| Status::Unavailable)?;
            let result = flight
                .ready
                .wait_timeout_while(result, Duration::from_secs(4), |result| result.is_none())
                .map_err(|_| Status::Unavailable)?;
            result
                .0
                .as_ref()
                .cloned()
                .unwrap_or(Err(Status::Unavailable))
        }
    }
}

struct ScreenCompletion<'a> {
    reads: &'a ScreenReads,
    target: &'a ScreenTarget,
    flight: &'a Arc<ScreenFlight>,
    pending: bool,
}

impl ScreenCompletion<'_> {
    fn publish(&mut self, result: ScreenResult) {
        let mut flights = self.reads.flights.lock().unwrap_or_else(|e| e.into_inner());
        let mut outcome = self.flight.result.lock().unwrap_or_else(|e| e.into_inner());
        *outcome = Some(result);
        if flights
            .get(self.target)
            .is_some_and(|entry| Arc::ptr_eq(entry, self.flight))
        {
            flights.remove(self.target);
        }
        self.pending = false;
        self.flight.ready.notify_all();
    }
}

impl Drop for ScreenCompletion<'_> {
    fn drop(&mut self) {
        if self.pending {
            self.publish(Err(Status::Unavailable));
        }
    }
}

fn resolve_screen(
    history: &mut HistoryStore,
    state: &Path,
    key: &SpaceKey,
    address_id: &str,
) -> Result<ScreenTarget, Status> {
    let store = history.store(state)?;
    let namespace = store
        .dashboard_spaces()
        .map_err(|_| Status::Unavailable)?
        .into_iter()
        .find(|space| SpaceKey::of(&space.namespace) == *key)
        .map(|space| space.namespace)
        .ok_or(Status::NotFound)?;
    match store
        .dashboard_screen_pane(&namespace, address_id)
        .map_err(|_| Status::Unavailable)?
    {
        ScreenPane::Missing => Err(Status::NotFound),
        ScreenPane::Gone => Err(Status::Gone),
        ScreenPane::Bound(pane) => Ok(ScreenTarget {
            socket: namespace.session_socket_id,
            pane,
        }),
    }
}

fn read_screen(target: &ScreenTarget) -> ScreenResult {
    let output = ProcessHerdr::default()
        .with_socket(Some(target.socket.clone()))
        .execute_bounded(
            &[
                "pane",
                "read",
                &target.pane,
                "--source",
                "recent-unwrapped",
                "--lines",
                "200",
            ],
            Duration::from_secs(3),
            &AtomicBool::new(false),
            1024 * 1024,
        )
        .map_err(|error| {
            if serde_json::from_str::<serde_json::Value>(&error)
                .ok()
                .is_some_and(|value| {
                    value.pointer("/error/code").and_then(|code| code.as_str())
                        == Some("pane_not_found")
                })
            {
                Status::Gone
            } else {
                Status::Unavailable
            }
        })?;
    let clean = strip_terminal_sequences(&output.stdout);
    let safe = snapshot::SafeText::from_untrusted(&clean, usize::MAX).text;
    Ok(newest_screen_text(&safe).into())
}

fn newest_screen_text(text: &str) -> String {
    const MAX_CHARS: usize = 64 * 1024;
    const MARKER: &str = "[Earlier screen text omitted]\n";
    if text.chars().count() <= MAX_CHARS {
        return text.to_owned();
    }
    let budget = MAX_CHARS - MARKER.chars().count();
    let (start, _) = text.char_indices().rev().nth(budget - 1).unwrap();
    let mut suffix = &text[start..];
    if !text[..start].ends_with('\n')
        && let Some((_, whole_lines)) = suffix.split_once('\n')
        && !whole_lines.is_empty()
    {
        suffix = whole_lines;
    }
    format!("{MARKER}{suffix}")
}

pub(super) fn strip_terminal_sequences(raw: &str) -> String {
    enum State {
        Ground,
        Escape,
        Csi,
        ControlString { osc: bool },
        StringEscape { osc: bool },
    }
    let mut state = State::Ground;
    let mut text = String::with_capacity(raw.len());
    for c in raw
        .chars()
        .filter(|c| !crate::domain::is_invisible_format(*c))
    {
        state = match state {
            State::Ground => match c {
                '\u{1b}' => State::Escape,
                '\u{9b}' => State::Csi,
                '\u{9d}' => State::ControlString { osc: true },
                '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => State::ControlString { osc: false },
                _ => {
                    if c == '\n' || c == '\t' || !c.is_control() {
                        text.push(c);
                    }
                    State::Ground
                }
            },
            State::Escape => match c {
                '\n' => State::Ground,
                '[' => State::Csi,
                ']' => State::ControlString { osc: true },
                'P' | 'X' | '^' | '_' => State::ControlString { osc: false },
                '\u{1b}' => State::Escape,
                '\u{30}'..='\u{7e}' => State::Ground,
                _ => State::Escape,
            },
            State::Csi => match c {
                '\u{40}'..='\u{7e}' => State::Ground,
                '\u{1b}' => State::Escape,
                _ => State::Csi,
            },
            State::ControlString { osc } => match c {
                '\u{9c}' => State::Ground,
                '\u{7}' if osc => State::Ground,
                '\u{1b}' => State::StringEscape { osc },
                _ => State::ControlString { osc },
            },
            State::StringEscape { osc } => match c {
                '\\' | '\u{9c}' => State::Ground,
                '\u{7}' if osc => State::Ground,
                '\u{1b}' => State::StringEscape { osc },
                _ => State::ControlString { osc },
            },
        };
    }
    text
}

struct ConnectionPermit(Arc<AtomicUsize>);

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

struct Request {
    method: String,
    path: String,
    query: Option<String>,
    headers: Vec<(String, String)>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    fn query(&self, name: &str) -> Option<&str> {
        self.query
            .as_deref()?
            .split('&')
            .find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='))
    }

    fn cookie(&self, name: &str) -> Option<&str> {
        self.header("cookie")?
            .split(';')
            .find_map(|pair| pair.trim().strip_prefix(name)?.strip_prefix('='))
    }
}

#[derive(Clone, Copy, Debug)]
enum Status {
    NoContent,
    BadRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    Gone,
    MethodNotAllowed,
    Unavailable,
    PayloadTooLarge,
    UnsupportedMedia,
    TooManyRequests,
}

impl Status {
    fn line(self) -> &'static str {
        match self {
            Self::NoContent => "204 No Content",
            Self::PayloadTooLarge => "413 Payload Too Large",
            Self::UnsupportedMedia => "415 Unsupported Media Type",
            Self::TooManyRequests => "429 Too Many Requests",
            Self::BadRequest => "400 Bad Request",
            Self::Unauthorized => "401 Unauthorized",
            Self::Forbidden => "403 Forbidden",
            Self::NotFound => "404 Not Found",
            Self::Gone => "410 Gone",
            Self::MethodNotAllowed => "405 Method Not Allowed",
            Self::Unavailable => "503 Service Unavailable",
        }
    }

    fn body(self) -> &'static str {
        match self {
            Self::NoContent => "",
            Self::Unauthorized => {
                "This browser isn't signed in to Channels. Your agents and messages are fine.\n\nOn the machine running Channels, run `namedrop channels` in a workspace and open the link it prints. Away from that machine? Run `namedrop channels --tailscale --no-open` there over SSH and open the HTTPS link on this device.\n"
            }
            Self::PayloadTooLarge => "Request body is too large.\n",
            Self::UnsupportedMedia => "Not a PNG, JPEG, GIF or WebP image.\n",
            Self::TooManyRequests => "Too many input requests.\n",
            Self::BadRequest => "Bad request.\n",
            Self::Forbidden => "Forbidden.\n",
            Self::NotFound => "Not found.\n",
            Self::Gone => "This participant's pane is no longer available.\n",
            Self::MethodNotAllowed => "Method not allowed.\n",
            Self::Unavailable => "History is temporarily unavailable.\n",
        }
    }
}

pub(crate) enum Assets {
    Embedded,
    Directory(PathBuf),
}

enum AssetBody {
    Embedded(&'static [u8]),
    File(File),
}

struct ResolvedAsset {
    body: AssetBody,
    length: u64,
    is_index: bool,
}

impl Assets {
    pub fn new(source: &UiSource) -> Self {
        match source {
            UiSource::Embedded => Self::Embedded,
            UiSource::Directory(path) => Self::Directory(path.clone()),
        }
    }

    fn resolve(&self, path: &str) -> Option<ResolvedAsset> {
        match self {
            Self::Embedded => {
                let bytes = channels_bundle::get(path)?;
                Some(ResolvedAsset {
                    body: AssetBody::Embedded(bytes),
                    length: bytes.len() as u64,
                    is_index: path == "/index.html",
                })
            }
            Self::Directory(path_root) => {
                let root = open_directory(path_root).ok()?;
                let file = open_asset(&root, path)?;
                let metadata = file.metadata().ok()?;
                let is_index = open_asset(&root, "/index.html")
                    .and_then(|index| index.metadata().ok())
                    .is_some_and(|index| {
                        (index.dev(), index.ino()) == (metadata.dev(), metadata.ino())
                    });
                Some(ResolvedAsset {
                    body: AssetBody::File(file),
                    length: metadata.len(),
                    is_index,
                })
            }
        }
    }
}

fn open_asset(root: &File, path: &str) -> Option<File> {
    let parts: Vec<_> = path.strip_prefix('/')?.split('/').collect();
    if parts
        .iter()
        .any(|part| part.is_empty() || part.starts_with('.'))
    {
        return None;
    }
    let mut file = root.try_clone().ok()?;
    for (index, part) in parts.iter().enumerate() {
        let name = CString::new(*part).ok()?;
        let flags = libc::O_RDONLY
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | if index + 1 == parts.len() {
                libc::O_NONBLOCK
            } else {
                libc::O_DIRECTORY
            };
        let fd = unsafe { libc::openat(file.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return None;
        }
        // openat returned a new owned descriptor.
        file = unsafe { File::from_raw_fd(fd) };
    }
    file.metadata().ok()?.is_file().then_some(file)
}

fn decode_asset_path(path: &str) -> Option<String> {
    let mut decoded = Vec::with_capacity(path.len());
    let mut bytes = path.bytes();
    while let Some(byte) = bytes.next() {
        let byte = if byte == b'%' {
            let high = (bytes.next()? as char).to_digit(16)?;
            let low = (bytes.next()? as char).to_digit(16)?;
            (high * 16 + low) as u8
        } else {
            byte
        };
        if byte == 0 || byte == b'\\' {
            return None;
        }
        decoded.push(byte);
    }
    String::from_utf8(decoded).ok()
}

fn history_query(request: &Request) -> Result<(Option<String>, i64), Status> {
    let mut before = None;
    let mut limit = None;
    for pair in request
        .query
        .as_deref()
        .unwrap_or("")
        .split('&')
        .filter(|p| !p.is_empty())
    {
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        if name != "before" && name != "limit" {
            continue;
        }
        let value = decode_query_value(value).ok_or(Status::BadRequest)?;
        if name == "before" {
            if before.replace(value).is_some() || before.as_deref() == Some("") {
                return Err(Status::BadRequest);
            }
        } else if limit.replace(value).is_some() {
            return Err(Status::BadRequest);
        }
    }
    let limit = limit
        .map(|value| value.parse::<i64>().map(|n| n.clamp(1, 100)))
        .transpose()
        .map_err(|_| Status::BadRequest)?
        .unwrap_or(50);
    Ok((before, limit))
}

fn decode_query_value(raw: &str) -> Option<String> {
    let mut decoded = Vec::with_capacity(raw.len());
    let mut bytes = raw.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = (bytes.next()? as char).to_digit(16)?;
            let low = (bytes.next()? as char).to_digit(16)?;
            decoded.push((high * 16 + low) as u8);
        } else {
            decoded.push(if byte == b'+' { b' ' } else { byte });
        }
    }
    String::from_utf8(decoded).ok()
}

fn content_type(path: &str) -> &'static str {
    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "json" | "map" => "application/json",
        "wasm" => "application/wasm",
        "ico" => "image/x-icon",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

enum Route {
    Reminders {
        key: SpaceKey,
        conversation: super::reminders::Conversation,
        cancel: Option<String>,
    },
    Redeem {
        location: String,
        nonce: String,
    },
    Page,
    Session,
    ImageUpload,
    Image(super::images::Name),
    Send(SpaceKey),
    DirectMessages,
    DirectHistory {
        key: SpaceKey,
        address_id: String,
        before: Option<String>,
        limit: i64,
    },
    DirectSend {
        key: SpaceKey,
        address_id: String,
    },
    CoordinationReview(SpaceKey),
    Deliver {
        key: SpaceKey,
        message_id: String,
    },
    ThreadControl {
        key: SpaceKey,
        thread_id: String,
        action: super::thread_controls::Action,
    },
    Dismiss {
        key: SpaceKey,
        attempt_id: String,
    },
    Skills {
        key: SpaceKey,
        address_id: String,
    },
    Screen {
        key: SpaceKey,
        address_id: String,
    },
    Live {
        key: SpaceKey,
        address_id: String,
        continuity: Option<String>,
    },
    LiveInput {
        key: SpaceKey,
        address_id: String,
    },
    Events(SpaceKey),
    Messages {
        key: SpaceKey,
        before: Option<String>,
        limit: i64,
    },
    MessageContext {
        key: SpaceKey,
        message_id: String,
    },
    MessageBody {
        key: SpaceKey,
        message_id: String,
        direct_peer: Option<crate::PaneAddressId>,
    },
    AllEvents,
    Spaces,
    Asset(String),
    Favicon,
    Health,
}

fn admit_host(
    request: &Request,
    port: u16,
    tailnet: Option<&super::tailscale::OwnedHandler>,
) -> Result<String, Status> {
    let host = request.header("host").ok_or(Status::Forbidden)?;
    // Exact loopback Host defeats DNS rebinding onto this port.
    if host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}") {
        // Serve preserves the client's Host but always adds proxy headers. Never let
        // a remote caller disguise a proxied request as direct loopback traffic.
        if request.headers.iter().any(|(name, _)| {
            name.starts_with("x-forwarded-")
                || name.starts_with("tailscale-")
                || name == "forwarded"
        }) {
            return Err(Status::Forbidden);
        }
        return Ok(format!("http://{host}"));
    }
    let handler = tailnet.ok_or(Status::Forbidden)?;
    // Serve removes client identity headers and adds the authenticated login:
    // https://tailscale.com/docs/features/tailscale-serve#identity-headers
    // Trust is limited to our loopback listener; local OS processes are already trusted.
    if host != handler.authority()
        || handler.backend_port != port
        || request.header("tailscale-user-login") != Some(handler.owner_login.as_str())
    {
        return Err(Status::Forbidden);
    }
    let origin = handler.origin();
    if request
        .header("origin")
        .is_some_and(|supplied| supplied != origin)
    {
        return Err(Status::Forbidden);
    }
    Ok(origin)
}

fn tailnet_still_enabled(front: &Front, expected: Option<&super::tailscale::OwnedHandler>) -> bool {
    expected.is_none_or(|expected| {
        matches!(super::tailscale::read(front.files.state()),
            Ok(Some(super::tailscale::Ownership::Enabled(current))) if current == *expected)
    })
}

fn route(
    request: &Request,
    port: u16,
    token: &Token,
    session: &Token,
    directory: bool,
    tailnet: Option<&super::tailscale::OwnedHandler>,
) -> Result<Route, Status> {
    let origin = admit_host(request, port, tailnet)?;
    if request.header("sec-fetch-site") == Some("cross-site") {
        return Err(Status::Forbidden);
    }
    if request.method != "GET" && request.method != "POST" {
        return Err(Status::MethodNotAllowed);
    }
    let path = request.path.as_str();
    if request.method == "POST"
        && matches!(
            path.split('/').collect::<Vec<_>>().as_slice(),
            ["", "w", _, "participants", _, "live" | "skills"]
        )
    {
        return Err(Status::MethodNotAllowed);
    }
    if path == "/api/images" {
        if request.method != "POST" {
            return Err(Status::MethodNotAllowed);
        }
        authenticate_session(request, port, session)?;
        authenticate_mutation(request, session, &origin)?;
        return Ok(Route::ImageUpload);
    }
    if let Some(name) = path.strip_prefix("/api/images/") {
        if request.method != "GET" {
            return Err(Status::MethodNotAllowed);
        }
        authenticate_session(request, port, session)?;
        authenticate_provenance(request, &origin)?;
        return super::images::Name::parse(name)
            .map(Route::Image)
            .ok_or(Status::NotFound);
    }
    if path == "/api/direct-messages" && request.method == "GET" {
        authenticate_session(request, port, session)?;
        authenticate_provenance(request, &origin)?;
        return Ok(Route::DirectMessages);
    }
    if let Some(rest) = path.strip_prefix("/w/") {
        let segments: Vec<_> = rest.split('/').collect();
        if matches!(segments.get(1), Some(&"threads" | &"direct-messages"))
            && segments.get(3) == Some(&"reminders")
        {
            authenticate_session(request, port, session)?;
            authenticate_provenance(request, &origin)?;
            let cancel = match (request.method.as_str(), segments.as_slice()) {
                ("GET", [_, _, _, "reminders"]) => None,
                ("POST", [_, _, _, "reminders", reminder, "cancel"]) => {
                    authenticate_mutation(request, session, &origin)?;
                    Some(decode_route_id(reminder)?)
                }
                _ => return Err(Status::MethodNotAllowed),
            };
            let key = SpaceKey::parse(segments[0]).ok_or(Status::NotFound)?;
            let id = decode_route_id(segments[2])?;
            let conversation = if segments[1] == "direct-messages" {
                super::reminders::Conversation::Direct(id)
            } else {
                super::reminders::Conversation::Thread(id)
            };
            return Ok(Route::Reminders {
                key,
                conversation,
                cancel,
            });
        }
        if let [key, "direct-messages", address, "messages", message, "body"] = segments.as_slice()
        {
            authenticate_session(request, port, session)?;
            authenticate_mutation(request, session, &origin)?;
            if request.method != "GET" {
                return Err(Status::MethodNotAllowed);
            }
            let key = SpaceKey::parse(key).ok_or(Status::NotFound)?;
            let address_id = decode_asset_path(address).ok_or(Status::NotFound)?;
            let message_id = decode_asset_path(message).ok_or(Status::NotFound)?;
            if address_id.is_empty()
                || address_id.contains('/')
                || message_id.is_empty()
                || message_id.contains('/')
            {
                return Err(Status::NotFound);
            }
            return Ok(Route::MessageBody {
                key,
                message_id,
                direct_peer: Some(crate::PaneAddressId::new(address_id)),
            });
        }
        if let [key, "direct-messages", address, "messages"] = segments.as_slice() {
            authenticate_session(request, port, session)?;
            authenticate_provenance(request, &origin)?;
            let key = SpaceKey::parse(key).ok_or(Status::NotFound)?;
            let address_id = decode_asset_path(address).ok_or(Status::NotFound)?;
            if address_id.is_empty() || address_id.contains('/') {
                return Err(Status::NotFound);
            }
            return if request.method == "POST" {
                authenticate_mutation(request, session, &origin)?;
                Ok(Route::DirectSend { key, address_id })
            } else {
                let (before, limit) = history_query(request)?;
                Ok(Route::DirectHistory {
                    key,
                    address_id,
                    before,
                    limit,
                })
            };
        }
    }
    if request.method == "POST" {
        authenticate_session(request, port, session)?;
        authenticate_mutation(request, session, &origin)?;
        if let Some(rest) = path.strip_prefix("/w/") {
            let segments: Vec<_> = rest.split('/').collect();
            if let [key, "participants", address, "input"] = segments.as_slice() {
                let key = SpaceKey::parse(key).ok_or(Status::NotFound)?;
                let address_id = decode_asset_path(address).ok_or(Status::NotFound)?;
                if address_id.is_empty() || address_id.contains('/') {
                    return Err(Status::NotFound);
                }
                return Ok(Route::LiveInput { key, address_id });
            }
            if let [key, "messages", message, "deliver"] = segments.as_slice() {
                let key = SpaceKey::parse(key).ok_or(Status::NotFound)?;
                let message_id = decode_asset_path(message).ok_or(Status::NotFound)?;
                if message_id.is_empty() || message_id.contains('/') {
                    return Err(Status::NotFound);
                }
                return Ok(Route::Deliver { key, message_id });
            }
            if let [key, "attempts", attempt, "dismiss"] = segments.as_slice() {
                let key = SpaceKey::parse(key).ok_or(Status::NotFound)?;
                let attempt_id = decode_asset_path(attempt).ok_or(Status::NotFound)?;
                if attempt_id.is_empty() || attempt_id.contains('/') {
                    return Err(Status::NotFound);
                }
                return Ok(Route::Dismiss { key, attempt_id });
            }
            if let [key, "coordinations", "review"] = segments.as_slice() {
                let key = SpaceKey::parse(key).ok_or(Status::NotFound)?;
                return Ok(Route::CoordinationReview(key));
            }
            if let [key, "threads", thread, action] = segments.as_slice() {
                let action = match *action {
                    "continue" => super::thread_controls::Action::Continue,
                    "stop" => super::thread_controls::Action::Stop,
                    _ => return Err(Status::NotFound),
                };
                let key = SpaceKey::parse(key).ok_or(Status::NotFound)?;
                let thread_id = decode_asset_path(thread).ok_or(Status::NotFound)?;
                if thread_id.is_empty() || thread_id.contains('/') {
                    return Err(Status::NotFound);
                }
                return Ok(Route::ThreadControl {
                    key,
                    thread_id,
                    action,
                });
            }
        }
        let key = path
            .strip_prefix("/w/")
            .and_then(|rest| rest.strip_suffix("/messages"))
            .and_then(SpaceKey::parse)
            .ok_or(Status::NotFound)?;
        return Ok(Route::Send(key));
    }
    if let Some(rest) = path.strip_prefix("/w/") {
        let segments: Vec<_> = rest.split('/').collect();
        if segments.get(1) == Some(&"messages") && segments.len() > 2 {
            authenticate_session(request, port, session)?;
            authenticate_provenance(request, &origin)?;
            let [key, "messages", message, kind @ ("context" | "body")] = segments.as_slice()
            else {
                return Err(Status::NotFound);
            };
            if *kind == "body" {
                authenticate_mutation(request, session, &origin)?;
            }
            let key = SpaceKey::parse(key).ok_or(Status::NotFound)?;
            let message_id = decode_asset_path(message).ok_or(Status::NotFound)?;
            if message_id.is_empty() {
                return Err(Status::NotFound);
            }
            return Ok(if *kind == "body" {
                Route::MessageBody {
                    key,
                    message_id,
                    direct_peer: None,
                }
            } else {
                Route::MessageContext { key, message_id }
            });
        }
        if segments.get(1) == Some(&"participants") {
            authenticate_session(request, port, session)?;
            authenticate_provenance(request, &origin)?;
            let [
                key,
                "participants",
                address,
                kind @ ("screen" | "live" | "skills"),
            ] = segments.as_slice()
            else {
                return Err(Status::NotFound);
            };
            let key = SpaceKey::parse(key).ok_or(Status::NotFound)?;
            let address_id = decode_asset_path(address).ok_or(Status::NotFound)?;
            if address_id.is_empty() || address_id.contains('/') {
                return Err(Status::NotFound);
            }
            return if *kind == "skills" {
                Ok(Route::Skills { key, address_id })
            } else if *kind == "live" {
                if request
                    .header("content-length")
                    .is_some_and(|length| length != "0")
                {
                    return Err(Status::BadRequest);
                }
                let continuity = live_peek::continuity_query(request.query.as_deref())?;
                Ok(Route::Live {
                    key,
                    address_id,
                    continuity,
                })
            } else {
                Ok(Route::Screen { key, address_id })
            };
        }
    }
    match path {
        "/healthz" => {
            return match request.query("t") {
                Some(candidate) if token.matches(candidate) => Ok(Route::Health),
                _ => Err(Status::Unauthorized),
            };
        }
        _ if path != "/index.html" && channels_bundle::get(path).is_some() => {
            return Ok(Route::Asset(path.to_owned()));
        }
        "/favicon.ico" if !directory => return Ok(Route::Favicon),
        _ => {}
    }
    let decoded = if directory {
        Some(decode_asset_path(path).ok_or(Status::NotFound)?)
    } else {
        None
    };
    let custom_index = decoded.as_deref() == Some("/index.html");
    if directory
        && path != "/"
        && !custom_index
        && !path.starts_with("/api/")
        && !path.starts_with("/w/")
    {
        let decoded = decoded.as_deref().ok_or(Status::NotFound)?;
        if decoded == "/healthz" || decoded.starts_with("/api/") || decoded.starts_with("/w/") {
            return Err(Status::NotFound);
        }
        return Ok(Route::Asset(decoded.to_owned()));
    }
    let page = match path {
        "/" | "/api/spaces" | "/api/events" | "/api/session" => None,
        _ if custom_index => None,
        _ => {
            let rest = path.strip_prefix("/w/").ok_or(Status::NotFound)?;
            let (segment, kind) = if let Some(segment) = rest.strip_suffix("/events") {
                (segment, "events")
            } else if let Some(segment) = rest.strip_suffix("/messages") {
                (segment, "messages")
            } else {
                (rest, "page")
            };
            Some((SpaceKey::parse(segment).ok_or(Status::NotFound)?, kind))
        }
    };
    if !matches!(page, Some((_, "messages")))
        && let Some(nonce) = request.query("n")
    {
        let query: Vec<_> = request
            .query
            .as_deref()
            .unwrap_or_default()
            .split('&')
            .filter(|pair| !pair.starts_with("n="))
            .collect();
        return Ok(Route::Redeem {
            location: if query.is_empty() {
                path.to_owned()
            } else {
                format!("{path}?{}", query.join("&"))
            },
            nonce: nonce.to_owned(),
        });
    }
    authenticate_session(request, port, session)?;
    if let Some((key, "messages")) = page {
        let (before, limit) = history_query(request)?;
        return Ok(Route::Messages { key, before, limit });
    }
    Ok(match (path, page) {
        ("/api/session", _) => Route::Session,
        ("/api/spaces", _) => Route::Spaces,
        ("/api/events", _) => Route::AllEvents,
        (_, Some((key, "events"))) => Route::Events(key),
        _ => Route::Page,
    })
}

fn decode_route_id(segment: &str) -> Result<String, Status> {
    decode_asset_path(segment)
        .filter(|id| !id.is_empty() && !id.contains('/'))
        .ok_or(Status::NotFound)
}

fn csrf_token(session: &Token) -> String {
    let mut digest = Sha256::new();
    digest.update(b"namedrop-dashboard-csrf/v1\0");
    digest.update(session.as_str().as_bytes());
    crate::lower_hex(&digest.finalize())
}

fn authenticate_session(request: &Request, port: u16, session: &Token) -> Result<(), Status> {
    if request
        .cookie(&cookie_name(port))
        .is_some_and(|candidate| session.matches(candidate))
    {
        Ok(())
    } else {
        Err(Status::Unauthorized)
    }
}

fn authenticate_mutation(request: &Request, session: &Token, origin: &str) -> Result<(), Status> {
    let expected = csrf_token(session);
    let candidate = request.header("x-namedrop-csrf").ok_or(Status::Forbidden)?;
    if expected.len() != candidate.len()
        || expected
            .bytes()
            .zip(candidate.bytes())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            != 0
    {
        return Err(Status::Forbidden);
    }
    authenticate_provenance(request, origin)
}

fn authenticate_provenance(request: &Request, expected_origin: &str) -> Result<(), Status> {
    if request
        .header("origin")
        .is_some_and(|origin| origin != expected_origin)
    {
        return Err(Status::Forbidden);
    }
    if request
        .header("sec-fetch-site")
        .is_some_and(|site| !matches!(site, "same-origin" | "none"))
    {
        return Err(Status::Forbidden);
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorPost {
    request_id: String,
    source: String,
    #[serde(default)]
    in_reply_to: Option<String>,
}

impl OperatorPost {
    fn read(reader: &mut impl Read, request: &Request) -> Result<Self, Status> {
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
        if length > BODY_LIMIT {
            return Err(Status::PayloadTooLarge);
        }
        let mut bytes = vec![0; length];
        reader
            .read_exact(&mut bytes)
            .map_err(|_| Status::BadRequest)?;
        let post: Self = serde_json::from_slice(&bytes).map_err(|_| Status::BadRequest)?;
        let valid_id = |id: &str| {
            !id.trim().is_empty() && id.len() <= 128 && !id.chars().any(char::is_control)
        };
        if !valid_id(&post.request_id)
            || post.source.trim().is_empty()
            || post.source.len() > BODY_LIMIT
            || post.in_reply_to.as_deref().is_some_and(|id| !valid_id(id))
            || too_many_images(&post.source)
        {
            return Err(Status::BadRequest);
        }
        Ok(post)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeliverPost {
    to: Vec<String>,
}

impl DeliverPost {
    fn read(reader: &mut impl Read, request: &Request) -> Result<Self, Status> {
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
        if length > BODY_LIMIT {
            return Err(Status::PayloadTooLarge);
        }
        let mut bytes = vec![0; length];
        reader
            .read_exact(&mut bytes)
            .map_err(|_| Status::BadRequest)?;
        let post: Self = serde_json::from_slice(&bytes).map_err(|_| Status::BadRequest)?;
        if post.to.is_empty() {
            return Err(Status::BadRequest);
        }
        Ok(post)
    }
}

fn too_many_images(source: &str) -> bool {
    super::images::split(source).1.len() > super::images::MAX_PER_MESSAGE
}

fn read_json_body<T: serde::de::DeserializeOwned>(
    reader: &mut impl Read,
    request: &Request,
) -> Result<T, Status> {
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
    if length > BODY_LIMIT {
        return Err(Status::PayloadTooLarge);
    }
    let mut bytes = vec![0; length];
    reader
        .read_exact(&mut bytes)
        .map_err(|_| Status::BadRequest)?;
    serde_json::from_slice(&bytes).map_err(|_| Status::BadRequest)
}

/// Cookies ignore ports, so the name carries it to keep two servers apart.
fn cookie_name(port: u16) -> String {
    format!("namedrop_dashboard_{port}")
}

fn session_cookie(front: &Front, tailnet: Option<&super::tailscale::OwnedHandler>) -> String {
    format!(
        "Set-Cookie: {}={}; HttpOnly; SameSite=Strict; Path=/{}\r\n",
        cookie_name(front.port),
        front.session.as_str(),
        if tailnet.is_some() {
            "; Secure; Max-Age=604800"
        } else {
            ""
        }
    )
}

/// A local cookie ends with the browser session, so only a tailnet cookie has a lifetime to renew.
fn renewed_session_cookie(
    front: &Front,
    tailnet: Option<&super::tailscale::OwnedHandler>,
) -> String {
    tailnet
        .map(|_| session_cookie(front, tailnet))
        .unwrap_or_default()
}

fn serve_connection(
    stream: TcpStream,
    front: &Front,
    next_id: &AtomicU64,
    history_store: &Mutex<HistoryStore>,
    screens: &ScreenReads,
    live: &live_peek::LivePeeks,
    skills: &super::skills::SkillCatalog,
) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(&stream);
    let request = match read_request(&mut reader, front.port) {
        Ok(request) => request,
        Err(status) => return respond_status(&mut writer, status),
    };
    let ownership = super::tailscale::read(front.files.state()).ok().flatten();
    let tailnet = match &ownership {
        Some(super::tailscale::Ownership::Enabled(handler)) => Some(handler),
        _ => None,
    };
    let tailnet_request =
        tailnet.filter(|handler| request.header("host") == Some(handler.authority().as_str()));
    let route = route(
        &request,
        front.port,
        &front.token,
        &front.session,
        matches!(front.assets, Assets::Directory(_)),
        tailnet,
    );
    let route = match route {
        Ok(Route::Redeem { ref nonce, .. })
            if !front.files.consume_entry(nonce)
                && authenticate_session(&request, front.port, &front.session).is_err() =>
        {
            Err(Status::Unauthorized)
        }
        route => route,
    };
    match route {
        Err(Status::Unauthorized) if tailnet_request.is_some() => respond(
            &mut writer,
            Status::Unauthorized.line(),
            "text/plain; charset=utf-8",
            "",
            b"This browser isn't signed in to Channels. Your agents and messages are fine.\n\nTo sign in, run this on the machine running Channels, then scan the QR code it draws with this device, or open the new HTTPS link here:\n\n    namedrop channels --tailscale --no-open\n\nNo terminal nearby? Connect to that machine over SSH from this device, or open Channels in a browser that's still signed in and ask an agent to run the command and send you the link. Otherwise, sign in when you're next at that machine.\n",
        ),
        Err(status) => respond_status(&mut writer, status),
        Ok(Route::Redeem { location, .. }) => {
            let headers = format!(
                "Location: {location}\r\n{}",
                session_cookie(front, tailnet_request)
            );
            respond(&mut writer, "303 See Other", "text/plain", &headers, b"")
        }
        Ok(Route::Session) => respond(
            &mut writer,
            "200 OK",
            "application/json",
            "",
            serde_json::json!({"csrf_token": csrf_token(&front.session)})
                .to_string()
                .as_bytes(),
        ),
        Ok(Route::ImageUpload) => upload_image(&mut reader, &mut writer, front, &request),
        Ok(Route::Image(name)) => serve_image(&mut writer, front, &name),
        Ok(Route::DirectMessages) => {
            let result = (|| {
                let mut guard = history_store.lock().map_err(|_| Status::Unavailable)?;
                match guard.store(front.files.state()) {
                    Ok(store) => store
                        .direct_message_peers()
                        .map_err(|_| Status::Unavailable),
                    Err(Status::NotFound) => Ok(Vec::new()),
                    Err(status) => Err(status),
                }
            })();
            match result {
                Ok(mut peers) => {
                    let (reply, answer) = mpsc::channel();
                    let spaces = front
                        .hub
                        .send(HubCommand::Spaces { reply })
                        .ok()
                        .and_then(|()| answer.recv_timeout(Duration::from_secs(5)).ok())
                        .and_then(|body| serde_json::from_str::<serde_json::Value>(&body).ok());
                    let mut live_spaces = HashMap::new();
                    for peer in &mut peers {
                        let live = if peer.closed {
                            None
                        } else {
                            live_spaces
                                .entry(peer.space_key.clone())
                                .or_insert_with(|| {
                                    let herdr = ProcessHerdr::default().with_socket(Some(
                                        peer.namespace.session_socket_id.clone(),
                                    ));
                                    crate::discover_worktree_for_workspace(
                                        &herdr,
                                        peer.namespace.space_id.as_str(),
                                    )
                                    .ok()
                                })
                                .as_ref()
                        };
                        if let Some(live) = live {
                            if let Some(label) = &live.label {
                                peer.workspace_label =
                                    snapshot::SafeText::from_untrusted(label, 256).text;
                            }
                            if let Some(pane) = live
                                .panes
                                .iter()
                                .find(|pane| Some(&pane.pane_id) == peer.pane_id.as_ref())
                            {
                                peer.pane_label = pane
                                    .pane_label
                                    .as_deref()
                                    .map(|label| snapshot::SafeText::from_untrusted(label, 256));
                                if let Some(runtime) = crate::integration::pane_agent_runtime(pane)
                                {
                                    peer.runtime = runtime.as_str().to_owned();
                                }
                                peer.presence = if matches!(
                                    pane.agent_status.as_deref(),
                                    Some("idle" | "done")
                                ) {
                                    "idle"
                                } else {
                                    "working"
                                }
                                .to_owned();
                            } else {
                                peer.closed = true;
                                peer.presence = "gone".to_owned();
                            }
                        } else {
                            peer.closed = true;
                            peer.presence = "gone".to_owned();
                        }
                        if let Some(label) = spaces
                            .as_ref()
                            .and_then(|v| v.as_array())
                            .and_then(|spaces| {
                                spaces.iter().find(|space| {
                                    space["key"].as_str() == Some(peer.space_key.as_str())
                                })
                            })
                            .and_then(|space| {
                                space["label"]["text"]
                                    .as_str()
                                    .or_else(|| space["workspace_id"].as_str())
                            })
                        {
                            peer.workspace_label =
                                snapshot::SafeText::from_untrusted(label, 256).text;
                        }
                    }
                    peers.sort_by(|a, b| {
                        b.last_activity_ms
                            .cmp(&a.last_activity_ms)
                            .then_with(|| a.workspace_label.cmp(&b.workspace_label))
                            .then_with(|| a.address_id.cmp(&b.address_id))
                    });
                    let body = serde_json::to_vec(&serde_json::json!({"peers":peers}))
                        .map_err(io::Error::other)?;
                    respond(&mut writer, "200 OK", "application/json", "", &body)
                }
                Err(status) => respond_status(&mut writer, status),
            }
        }
        Ok(Route::DirectHistory {
            key,
            address_id,
            before,
            limit,
        }) => {
            let result = (|| {
                let mut guard = history_store.lock().map_err(|_| Status::Unavailable)?;
                let store = guard.store(front.files.state())?;
                let ns = store
                    .dashboard_spaces()
                    .map_err(|_| Status::Unavailable)?
                    .into_iter()
                    .find(|space| SpaceKey::of(&space.namespace) == key)
                    .map(|space| space.namespace)
                    .ok_or(Status::NotFound)?;
                store
                    .direct_message_history(
                        &ns,
                        &crate::PaneAddressId::new(address_id),
                        before.as_deref(),
                        limit,
                    )
                    .map_err(|_| Status::Unavailable)?
                    .ok_or(Status::NotFound)
            })();
            match result {
                Ok(rows) => {
                    let messages: Vec<_> = rows
                        .history
                        .messages
                        .iter()
                        .map(|row| snapshot::message(row, &rows.history.legs))
                        .collect();
                    let body=serde_json::to_vec(&serde_json::json!({"thread_id":rows.thread_id,"messages":messages,"earlier_omitted":rows.earlier_omitted})).map_err(io::Error::other)?;
                    respond(&mut writer, "200 OK", "application/json", "", &body)
                }
                Err(status) => respond_status(&mut writer, status),
            }
        }
        Ok(Route::DirectSend { key, address_id }) => {
            let post: DirectMessagePost = match read_json_body(&mut reader, &request) {
                Ok(post) => post,
                Err(status) => return respond_status(&mut writer, status),
            };
            if post.request_id.trim().is_empty()
                || post.request_id.len() > 128
                || post.request_id.chars().any(char::is_control)
                || post.source.trim().is_empty()
                || post.source.len() > BODY_LIMIT
                || too_many_images(&post.source)
            {
                return respond_status(&mut writer, Status::BadRequest);
            }
            send_direct_post(&mut writer, front, key, &address_id, post)
        }
        Ok(Route::Send(key)) => {
            let post = match OperatorPost::read(&mut reader, &request) {
                Ok(post) => post,
                Err(status) => return respond_status(&mut writer, status),
            };
            send_operator_post(&mut writer, front, key, post)
        }
        Ok(Route::CoordinationReview(key)) => {
            let post = match read_json_body(&mut reader, &request) {
                Ok(post) => post,
                Err(status) => return respond_status(&mut writer, status),
            };
            send_coordination_review(&mut writer, front, &key, &post)
        }
        Ok(Route::Deliver { key, message_id }) => {
            let post = match DeliverPost::read(&mut reader, &request) {
                Ok(post) => post,
                Err(status) => return respond_status(&mut writer, status),
            };
            let admitted = match front.admit_mutation() {
                Ok(guard) => guard,
                Err(status) => return respond_status(&mut writer, status),
            };
            writer.set_write_timeout(Some(Duration::from_secs(1)))?;
            match super::channel_delivery::apply(front.files.state(), &key, &message_id, &post.to) {
                Ok((namespace, receipt)) => {
                    let response = respond(
                        &mut writer,
                        "200 OK",
                        "application/json",
                        "",
                        receipt.to_string().as_bytes(),
                    );
                    drop(admitted);
                    super::channel_service::drain(front.files.state(), &namespace);
                    response
                }
                Err(error) => {
                    use super::channel_delivery::DeliverError;
                    use crate::store::channel_routing::ChannelDeliveryRefusal;
                    let (status, message) = match error {
                        DeliverError::Refused(ChannelDeliveryRefusal::NotFound) => {
                            ("404 Not Found", "Channel post not found.")
                        }
                        DeliverError::Refused(ChannelDeliveryRefusal::InvalidMember) => (
                            "400 Bad Request",
                            "Choose current agent members of this channel.",
                        ),
                        DeliverError::Refused(ChannelDeliveryRefusal::Routing) => (
                            "409 Conflict",
                            "This post is still routing. Try again when routing finishes.",
                        ),
                        DeliverError::Refused(ChannelDeliveryRefusal::Stopped) => {
                            ("409 Conflict", "This thread has stopped.")
                        }
                        DeliverError::Unavailable => (
                            "503 Service Unavailable",
                            "Channel delivery is unavailable.",
                        ),
                    };
                    respond(
                        &mut writer,
                        status,
                        "application/json",
                        "",
                        serde_json::json!({"error": message}).to_string().as_bytes(),
                    )
                }
            }
        }
        Ok(Route::Dismiss { key, attempt_id }) => {
            let post: super::acknowledge::DismissPost = match read_json_body(&mut reader, &request)
            {
                Ok(post) => post,
                Err(status) => return respond_status(&mut writer, status),
            };
            let admitted = match front.admit_mutation() {
                Ok(guard) => guard,
                Err(status) => return respond_status(&mut writer, status),
            };
            let result = super::acknowledge::dismiss(
                front.files.state(),
                &key,
                &attempt_id,
                &post,
                crate::Clock::now_unix_ms(&crate::SystemClock),
            );
            drop(admitted);
            match result {
                Ok(receipt) => respond(
                    &mut writer,
                    "200 OK",
                    "application/json",
                    "",
                    receipt.to_string().as_bytes(),
                ),
                Err(super::acknowledge::Error::NotFound) => {
                    respond_status(&mut writer, Status::NotFound)
                }
                Err(super::acknowledge::Error::NothingToDismiss) => respond(
                    &mut writer,
                    "410 Gone",
                    "application/json",
                    "",
                    serde_json::json!({"error": "This request has no warning to dismiss anymore."})
                        .to_string()
                        .as_bytes(),
                ),
                Err(super::acknowledge::Error::Refused(message)) => respond(
                    &mut writer,
                    "409 Conflict",
                    "application/json",
                    "",
                    serde_json::json!({"error": message}).to_string().as_bytes(),
                ),
                Err(super::acknowledge::Error::Unavailable) => {
                    respond_status(&mut writer, Status::Unavailable)
                }
            }
        }
        Ok(Route::Reminders {
            key,
            conversation,
            cancel,
        }) => {
            let result = if let Some(reminder) = cancel {
                let _admitted = match front.admit_mutation() {
                    Ok(guard) => guard,
                    Err(status) => return respond_status(&mut writer, status),
                };
                super::reminders::cancel(
                    front.files.state(),
                    &key,
                    &conversation,
                    &reminder,
                    crate::Clock::now_unix_ms(&crate::SystemClock),
                )
            } else {
                super::reminders::list(front.files.state(), &key, &conversation)
                    .map(|reminders| serde_json::json!({ "reminders": reminders }))
            };
            match result {
                Ok(value) => respond(
                    &mut writer,
                    "200 OK",
                    "application/json",
                    "",
                    value.to_string().as_bytes(),
                ),
                Err(super::reminders::Error::NotFound) => {
                    respond_status(&mut writer, Status::NotFound)
                }
                Err(super::reminders::Error::Unavailable) => {
                    respond_status(&mut writer, Status::Unavailable)
                }
            }
        }
        Ok(Route::ThreadControl {
            key,
            thread_id,
            action,
        }) => {
            let admitted = match front.admit_mutation() {
                Ok(guard) => guard,
                Err(status) => return respond_status(&mut writer, status),
            };
            writer.set_write_timeout(Some(Duration::from_secs(1)))?;
            let completion = front.control_effects.track();
            match super::thread_controls::commit(front.files.state(), &key, &thread_id, action) {
                Ok(committed) => {
                    drop(admitted);
                    committed.respond(
                        front.files.state(),
                        completion,
                        |receipt| {
                            respond(
                                &mut writer,
                                "200 OK",
                                "application/json",
                                "",
                                receipt.to_string().as_bytes(),
                            )
                        },
                        |namespace| super::channel_service::drain(front.files.state(), namespace),
                    )
                }
                Err(super::thread_controls::ControlError::NotFound) => {
                    drop(completion);
                    drop(admitted);
                    respond_status(&mut writer, Status::NotFound)
                }
                Err(super::thread_controls::ControlError::Unavailable) => {
                    drop(completion);
                    drop(admitted);
                    respond_status(&mut writer, Status::Unavailable)
                }
            }
        }
        Ok(Route::Live {
            key,
            address_id,
            continuity,
        }) => {
            if !reader.buffer().is_empty() {
                return respond_status(&mut writer, Status::BadRequest);
            }
            drop(reader);
            drop(writer);
            live.serve(
                stream,
                front,
                &key,
                &address_id,
                continuity.as_deref(),
                tailnet_request,
            )
        }
        Ok(Route::LiveInput { key, address_id }) => {
            let post = match live_peek::InputPost::read(&mut reader, &request) {
                Ok(post) => post,
                Err(status) => return respond_status(&mut writer, status),
            };
            let result = live.send_input(front, &key, &address_id, post);
            respond_status(&mut writer, result.err().unwrap_or(Status::NoContent))
        }
        Ok(Route::Skills { key, address_id }) => {
            match skills_api::read(
                history_store,
                front.files.state(),
                &key,
                &address_id,
                skills,
            ) {
                Ok(reply) => respond(
                    &mut writer,
                    "200 OK",
                    "application/json",
                    "",
                    serde_json::to_string(&reply).unwrap().as_bytes(),
                ),
                Err(status) => respond_status(&mut writer, status),
            }
        }
        Ok(Route::Screen { key, address_id }) => {
            let _permit = match screens.try_admit() {
                Ok(permit) => permit,
                Err(status) => {
                    return respond(
                        &mut writer,
                        status.line(),
                        "text/plain; charset=utf-8",
                        "",
                        b"Screen is temporarily unavailable.\n",
                    );
                }
            };
            let result = (|| {
                let target = {
                    let mut guard = history_store.lock().map_err(|_| Status::Unavailable)?;
                    resolve_screen(&mut guard, front.files.state(), &key, &address_id)?
                };
                screens.read(target)
            })();
            match result {
                Ok(text) => respond(
                    &mut writer,
                    "200 OK",
                    "text/plain; charset=utf-8",
                    "",
                    text.as_bytes(),
                ),
                Err(Status::Unavailable) => respond(
                    &mut writer,
                    Status::Unavailable.line(),
                    "text/plain; charset=utf-8",
                    "",
                    b"Screen is temporarily unavailable.\n",
                ),
                Err(status) => respond_status(&mut writer, status),
            }
        }
        Ok(Route::Page) => serve_asset(
            &mut writer,
            front,
            &request,
            "/index.html",
            &renewed_session_cookie(front, tailnet_request),
        ),
        Ok(Route::Asset(path)) => serve_asset(&mut writer, front, &request, &path, ""),
        Ok(Route::Favicon) => respond(&mut writer, "204 No Content", "text/plain", "", b""),
        Ok(Route::Health) => respond(
            &mut writer,
            "200 OK",
            "application/json",
            "",
            serde_json::json!({
                "instance": front.instance,
                "restarting": front.restarting.load(Ordering::Acquire) || front.files.restarting(),
            })
            .to_string()
            .as_bytes(),
        ),
        Ok(Route::Spaces) => {
            let (reply, answer) = mpsc::channel();
            let body = front
                .hub
                .send(HubCommand::Spaces { reply })
                .ok()
                .and_then(|()| answer.recv_timeout(Duration::from_secs(5)).ok())
                .unwrap_or_else(|| "[]".to_owned());
            respond(
                &mut writer,
                "200 OK",
                "application/json",
                "",
                body.as_bytes(),
            )
        }
        Ok(Route::Events(key)) => stream_events(
            stream,
            writer,
            front,
            next_id,
            Subscription::Space(key),
            tailnet_request,
        ),
        Ok(Route::Messages { key, before, limit }) => {
            let result = (|| {
                let mut guard = history_store.lock().map_err(|_| Status::Unavailable)?;
                let store = guard.store(front.files.state())?;
                let namespace = store
                    .dashboard_spaces()
                    .map_err(|_| Status::Unavailable)?
                    .into_iter()
                    .find(|space| SpaceKey::of(&space.namespace) == key)
                    .map(|space| space.namespace)
                    .ok_or(Status::NotFound)?;
                store
                    .dashboard_history(&namespace, before.as_deref(), limit)
                    .map_err(|_| Status::Unavailable)?
                    .ok_or(Status::NotFound)
                    .map(|rows| (namespace, rows))
            })();
            match result {
                Ok((namespace, rows)) => {
                    let mut messages: Vec<_> = rows
                        .messages
                        .iter()
                        .map(|row| snapshot::message(row, &rows.legs))
                        .collect();
                    snapshot::channel_routing::merge(
                        front.files.state(),
                        &namespace,
                        &mut messages,
                    );
                    let body = serde_json::to_vec(&messages).map_err(io::Error::other)?;
                    respond(&mut writer, "200 OK", "application/json", "", &body)
                }
                Err(status) => respond_status(&mut writer, status),
            }
        }
        Ok(Route::MessageContext { key, message_id }) => {
            let result = (|| {
                let mut guard = history_store.lock().map_err(|_| Status::Unavailable)?;
                let store = guard.store(front.files.state())?;
                let namespace = store
                    .dashboard_spaces()
                    .map_err(|_| Status::Unavailable)?
                    .into_iter()
                    .find(|space| SpaceKey::of(&space.namespace) == key)
                    .map(|space| space.namespace)
                    .ok_or(Status::NotFound)?;
                store
                    .dashboard_thread_context(&namespace, &message_id)
                    .map_err(|_| Status::Unavailable)?
                    .ok_or(Status::NotFound)
                    .map(|context| (namespace, context))
            })();
            match result {
                Ok((namespace, context)) => {
                    let mut messages: Vec<_> = context
                        .history
                        .messages
                        .iter()
                        .map(|row| snapshot::message(row, &context.history.legs))
                        .collect();
                    snapshot::channel_routing::merge(
                        front.files.state(),
                        &namespace,
                        &mut messages,
                    );
                    let body = serde_json::to_vec(&serde_json::json!({
                        "thread_id": context.thread_id,
                        "message_count": context.message_count,
                        "earlier_omitted": context.earlier_omitted,
                        "messages": messages,
                    }))
                    .map_err(io::Error::other)?;
                    respond(&mut writer, "200 OK", "application/json", "", &body)
                }
                Err(status) => respond_status(&mut writer, status),
            }
        }
        Ok(Route::MessageBody {
            key,
            message_id,
            direct_peer,
        }) => {
            let result = (|| {
                let mut guard = history_store.lock().map_err(|_| Status::Unavailable)?;
                let store = guard.store(front.files.state())?;
                let namespace = store
                    .dashboard_spaces()
                    .map_err(|_| Status::Unavailable)?
                    .into_iter()
                    .find(|space| SpaceKey::of(&space.namespace) == key)
                    .map(|space| space.namespace)
                    .ok_or(Status::NotFound)?;
                match direct_peer {
                    Some(peer) => store.direct_message_body(
                        &namespace,
                        &peer,
                        &message_id,
                        BODY_LIMIT + BODY_READ_MARGIN,
                    ),
                    None => store.dashboard_message_body(
                        &namespace,
                        &message_id,
                        BODY_LIMIT + BODY_READ_MARGIN,
                    ),
                }
                .map_err(|_| Status::Unavailable)?
                .ok_or(Status::NotFound)
            })();
            match result {
                Ok(body) => {
                    // The row already lists the images; a cut body has lost their lines.
                    let text = if body.cut {
                        body.text.as_str()
                    } else {
                        super::images::split(&body.text).0
                    };
                    let mut safe = snapshot::SafeText::from_untrusted(text, BODY_LIMIT);
                    safe.truncated |= body.cut;
                    let body = serde_json::to_vec(&serde_json::json!({
                        "message_id": message_id,
                        "body": safe,
                    }))
                    .map_err(io::Error::other)?;
                    respond(&mut writer, "200 OK", "application/json", "", &body)
                }
                Err(status) => respond_status(&mut writer, status),
            }
        }
        Ok(Route::AllEvents) => stream_events(
            stream,
            writer,
            front,
            next_id,
            Subscription::All,
            tailnet_request,
        ),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectMessagePost {
    request_id: String,
    source: String,
}

fn send_direct_post(
    writer: &mut TcpStream,
    front: &Front,
    key: SpaceKey,
    address: &str,
    post: DirectMessagePost,
) -> io::Result<()> {
    let mut admitted = Some(match front.admit_mutation() {
        Ok(guard) => guard,
        Err(status) => return respond_status(writer, status),
    });
    let namespace = StateStore::open_read_only(front.files.state())
        .and_then(|store| store.map(|mut store| store.dashboard_spaces()).transpose());
    let namespace = match namespace {
        Ok(Some(spaces)) => spaces
            .into_iter()
            .find(|space| SpaceKey::of(&space.namespace) == key)
            .map(|space| space.namespace),
        Ok(None) => None,
        Err(_) => return respond_status(writer, Status::Unavailable),
    };
    let Some(namespace) = namespace else {
        return respond_status(writer, Status::NotFound);
    };
    let herdr = ProcessHerdr::default().with_socket(Some(namespace.session_socket_id.clone()));
    let ids = crate::ProcessIds::default();
    let runtime = crate::LifecycleRuntime {
        state_directory: front.files.state(),
        home_directory: None,
        agent_config: crate::AgentConfigDirectories::default(),
        cwd: Path::new("/"),
        workspace_id: Some(namespace.space_id.as_str()),
        current_pane_id: None,
        session_socket_id: &namespace.session_socket_id,
        herdr: &herdr,
        clock: &crate::SystemClock,
        ids: &ids,
    };
    writer.set_write_timeout(Some(Duration::from_secs(1)))?;
    let sent = crate::message::send_operator_direct_message_observing_commit(
        &post.request_id,
        &post.source,
        &crate::PaneAddressId::new(address),
        &runtime,
        |receipt| {
            front.channel_wake.notify();
            let response = respond(
                writer,
                "200 OK",
                "application/json",
                "",
                receipt.to_string().as_bytes(),
            );
            drop(admitted.take());
            response
        },
    );
    match sent {
        Ok((receipt, response)) => {
            if let Err(error) = wake_mailboxes_after_send(front.files.state(), &namespace, &receipt)
            {
                eprintln!("Message saved; could not wake delivery: {error}");
            }
            response
        }
        Err(error) => respond(
            writer,
            match error.kind() {
                crate::OperatorSendErrorKind::InvalidInput => "400 Bad Request",
                crate::OperatorSendErrorKind::Unavailable => "503 Service Unavailable",
                crate::OperatorSendErrorKind::Internal => "500 Internal Server Error",
            },
            "application/json",
            "",
            serde_json::json!({"error":error.public_message()})
                .to_string()
                .as_bytes(),
        ),
    }
}

fn send_operator_post(
    writer: &mut TcpStream,
    front: &Front,
    key: SpaceKey,
    post: OperatorPost,
) -> io::Result<()> {
    let mut admitted = Some(match front.admit_mutation() {
        Ok(guard) => guard,
        Err(status) => return respond_status(writer, status),
    });
    let namespace = match StateStore::open_read_only(front.files.state())
        .and_then(|store| store.map(|mut store| store.dashboard_spaces()).transpose())
    {
        Ok(Some(spaces)) => spaces
            .into_iter()
            .find(|space| SpaceKey::of(&space.namespace) == key)
            .map(|space| space.namespace),
        Ok(None) => return respond_status(writer, Status::NotFound),
        Err(_) => return respond_status(writer, Status::Unavailable),
    };
    let Some(namespace) = namespace else {
        return respond_status(writer, Status::NotFound);
    };
    let herdr =
        crate::ProcessHerdr::default().with_socket(Some(namespace.session_socket_id.clone()));
    let ids = crate::ProcessIds::default();
    let runtime = crate::LifecycleRuntime {
        state_directory: front.files.state(),
        home_directory: None,
        agent_config: crate::AgentConfigDirectories::default(),
        cwd: Path::new("/"),
        workspace_id: Some(namespace.space_id.as_str()),
        current_pane_id: None,
        session_socket_id: &namespace.session_socket_id,
        herdr: &herdr,
        clock: &crate::SystemClock,
        ids: &ids,
    };
    let target = post.in_reply_to.map(crate::MessageId::new);
    writer.set_write_timeout(Some(Duration::from_secs(1)))?;
    let observe_commit = |receipt: &serde_json::Value| {
        front.channel_wake.notify();
        let mut receipt = receipt.clone();
        snapshot::channel_routing::finish_post(front.files.state(), &namespace, &mut receipt);
        let response = respond(
            writer,
            "200 OK",
            "application/json",
            "",
            receipt.to_string().as_bytes(),
        );
        drop(admitted.take());
        response
    };
    let sent = if crate::message::is_plain_channel_post(&post.source) {
        crate::message::send_operator_channel_message_observing_commit(
            &post.request_id,
            &post.source,
            target.as_ref(),
            &runtime,
            observe_commit,
        )
    } else {
        crate::message::send_operator_message_observing_commit(
            &post.request_id,
            &post.source,
            target.as_ref(),
            None,
            &runtime,
            observe_commit,
        )
    };
    match sent {
        Ok((receipt, response)) => {
            if let Err(error) = wake_mailboxes_after_send(front.files.state(), &namespace, &receipt)
            {
                eprintln!("Message saved; could not wake delivery: {error}");
            }
            response
        }
        Err(error) => respond(
            writer,
            match error.kind() {
                crate::OperatorSendErrorKind::InvalidInput => "400 Bad Request",
                crate::OperatorSendErrorKind::Unavailable => "503 Service Unavailable",
                crate::OperatorSendErrorKind::Internal => "500 Internal Server Error",
            },
            "application/json",
            "",
            serde_json::json!({"error": error.public_message()})
                .to_string()
                .as_bytes(),
        ),
    }
}

fn send_coordination_review(
    writer: &mut TcpStream,
    front: &Front,
    key: &SpaceKey,
    post: &super::coordination_review::ReviewPost,
) -> io::Result<()> {
    use super::coordination_review::{ReviewError, commit};
    let mut admitted = Some(match front.admit_mutation() {
        Ok(guard) => guard,
        Err(status) => return respond_status(writer, status),
    });
    writer.set_write_timeout(Some(Duration::from_secs(1)))?;
    let operator =
        crate::store::coordination_review::OperatorSession::of_secret(front.session.as_str());
    let now = crate::Clock::now_unix_ms(&crate::SystemClock);
    let draft = match commit(front.files.state(), key, post, &operator, now) {
        Ok(draft) => draft,
        Err(ReviewError::NotFound) => return respond_status(writer, Status::NotFound),
        Err(ReviewError::Unavailable) => return respond_status(writer, Status::Unavailable),
        Err(ReviewError::Refused(message)) => {
            return respond(
                writer,
                "409 Conflict",
                "application/json",
                "",
                serde_json::json!({"error": message}).to_string().as_bytes(),
            );
        }
    };
    let namespace = &draft.namespace;
    let herdr =
        crate::ProcessHerdr::default().with_socket(Some(namespace.session_socket_id.clone()));
    let ids = crate::ProcessIds::default();
    let runtime = crate::LifecycleRuntime {
        state_directory: front.files.state(),
        home_directory: None,
        agent_config: crate::AgentConfigDirectories::default(),
        cwd: Path::new("/"),
        workspace_id: Some(namespace.space_id.as_str()),
        current_pane_id: None,
        session_socket_id: &namespace.session_socket_id,
        herdr: &herdr,
        clock: &crate::SystemClock,
        ids: &ids,
    };
    let granted: Vec<_> = draft
        .granted
        .iter()
        .map(|(id, expires)| serde_json::json!({"coordination_id": id, "expires_at_ms": expires}))
        .collect();
    let sent = crate::message::send_operator_message_observing_commit(
        &post.request_id,
        &draft.source,
        None,
        Some(&operator),
        &runtime,
        |receipt: &serde_json::Value| {
            front.channel_wake.notify();
            let mut receipt = receipt.clone();
            receipt["granted"] = serde_json::json!(granted);
            let response = respond(
                writer,
                "200 OK",
                "application/json",
                "",
                receipt.to_string().as_bytes(),
            );
            drop(admitted.take());
            response
        },
    );
    match sent {
        Ok((receipt, response)) => {
            if let Err(error) = wake_mailboxes_after_send(front.files.state(), namespace, &receipt)
            {
                eprintln!("Review request saved; could not wake delivery: {error}");
            }
            response
        }
        Err(error) => respond(
            writer,
            match error.kind() {
                crate::OperatorSendErrorKind::InvalidInput => "400 Bad Request",
                crate::OperatorSendErrorKind::Unavailable => "503 Service Unavailable",
                crate::OperatorSendErrorKind::Internal => "500 Internal Server Error",
            },
            "application/json",
            "",
            serde_json::json!({"error": error.public_message()})
                .to_string()
                .as_bytes(),
        ),
    }
}

pub(super) fn wake_mailboxes_after_send(
    state: &Path,
    namespace: &crate::StateNamespace,
    receipt: &serde_json::Value,
) -> Result<(), String> {
    use std::os::unix::process::CommandExt;

    let pending = crate::drain_reports(receipt)
        .filter(|(_, report)| report.needs_mailbox_worker())
        .map(|(workspace, _)| workspace)
        .collect::<Vec<_>>();
    let mut spaces = Vec::new();
    if pending.contains(&None) {
        spaces.push(namespace.space_id.clone());
    }
    if pending.iter().any(Option::is_some) {
        let message = crate::MessageId::new(
            receipt["message_id"]
                .as_str()
                .ok_or("send receipt has no message ID")?,
        );
        let mut store = StateStore::open(state).map_err(|error| error.to_string())?;
        let copies = store
            .transaction(|tx| tx.forwarded_copies(namespace, &message))
            .map_err(|error| error.to_string())?;
        spaces.extend(copies.into_iter().filter_map(|copy| {
            pending
                .contains(&Some(copy.workspace.as_str()))
                .then_some(copy.space)
        }));
    }
    for space in spaces {
        let mut child = Command::new(crate::runtime_executable()?)
            .args(["hook", "mailbox-wake", "--state-directory"])
            .arg(state)
            .env("HERDR_PLUGIN_STATE_DIR", state)
            .env("HERDR_SOCKET_PATH", &namespace.session_socket_id)
            .env("HERDR_WORKSPACE_ID", space.as_str())
            .env_remove("HERDR_PANE_ID")
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| error.to_string())?;
        thread::spawn(move || {
            if let Err(error) = child.wait() {
                eprintln!("could not reap mailbox wake: {error}");
            }
        });
    }
    Ok(())
}

struct UploadPermit<'a>(&'a AtomicUsize);

impl Drop for UploadPermit<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Stores the posted bytes when they are one of the four image types, and
/// answers with the line a message carries for the image.
fn upload_image(
    reader: &mut impl Read,
    writer: &mut TcpStream,
    front: &Front,
    request: &Request,
) -> io::Result<()> {
    let length = match request
        .header("content-length")
        .filter(|length| !length.is_empty() && length.bytes().all(|byte| byte.is_ascii_digit()))
    {
        Some(length) => length.parse::<usize>().unwrap_or(usize::MAX),
        None => return respond_status(writer, Status::BadRequest),
    };
    if length > super::images::MAX_BYTES {
        return respond_status(writer, Status::PayloadTooLarge);
    }
    if front.image_uploads.fetch_add(1, Ordering::AcqRel) >= MAX_IMAGE_UPLOADS {
        front.image_uploads.fetch_sub(1, Ordering::AcqRel);
        return respond_status(writer, Status::TooManyRequests);
    }
    let _upload = UploadPermit(&front.image_uploads);
    let mut bytes = vec![0; length];
    if reader.read_exact(&mut bytes).is_err() {
        return respond_status(writer, Status::BadRequest);
    }
    let state = front.files.state();
    let now = std::time::SystemTime::now();
    match super::images::save(state, &bytes, now) {
        Ok(name) => {
            super::images::prune(state, now);
            respond(
                writer,
                "200 OK",
                "application/json",
                "",
                serde_json::json!({
                    "name": name.as_str(),
                    "line": super::images::line(state, &name),
                })
                .to_string()
                .as_bytes(),
            )
        }
        Err(super::images::SaveError::NotAnImage) => {
            respond_status(writer, Status::UnsupportedMedia)
        }
        Err(super::images::SaveError::Io(error)) => {
            eprintln!("Could not store an image: {error}");
            respond_status(writer, Status::Unavailable)
        }
    }
}

fn serve_image(
    writer: &mut TcpStream,
    front: &Front,
    name: &super::images::Name,
) -> io::Result<()> {
    writer.set_write_timeout(Some(Duration::from_secs(5)))?;
    let Some((file, length)) = super::images::open(front.files.state(), name) else {
        return respond_status(writer, Status::NotFound);
    };
    write!(
        writer,
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {length}\r\n{IMAGE_HEADERS}Connection: close\r\n\r\n",
        name.kind().content_type(),
    )?;
    io::copy(&mut file.take(length), writer)?;
    writer.flush()
}

fn serve_asset(
    writer: &mut TcpStream,
    front: &Front,
    request: &Request,
    path: &str,
    extra_headers: &str,
) -> io::Result<()> {
    writer.set_write_timeout(Some(Duration::from_secs(1)))?;
    let Some(asset) = front.assets.resolve(path) else {
        return respond_status(writer, Status::NotFound);
    };
    if asset.is_index
        && !request
            .cookie(&cookie_name(front.port))
            .is_some_and(|candidate| front.session.matches(candidate))
    {
        return respond_status(writer, Status::Unauthorized);
    }
    write!(
        writer,
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\n{SECURITY_HEADERS}{}{extra_headers}Connection: close\r\n\r\n",
        content_type(path),
        asset.length,
        if asset.is_index {
            format!("X-Namedrop-Csrf: {}\r\n", csrf_token(&front.session))
        } else {
            String::new()
        }
    )?;
    match asset.body {
        AssetBody::Embedded(bytes) => writer.write_all(bytes)?,
        AssetBody::File(file) => {
            let mut reader = file.take(asset.length);
            let mut chunk = [0u8; 16 * 1024];
            loop {
                let count = reader.read(&mut chunk)?;
                if count == 0 {
                    break;
                }
                writer.write_all(&chunk[..count])?;
            }
        }
    }
    writer.flush()
}

fn stream_events(
    stream: TcpStream,
    mut writer: TcpStream,
    front: &Front,
    next_id: &AtomicU64,
    subscription: Subscription,
    tailnet: Option<&super::tailscale::OwnedHandler>,
) -> io::Result<()> {
    let renewal = renewed_session_cookie(front, tailnet);
    write!(
        writer,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n{SECURITY_HEADERS}{renewal}Connection: close\r\n\r\nretry: 1000\n\n"
    )?;
    writer.flush()?;
    let id = next_id.fetch_add(1, Ordering::Relaxed);
    let (frames, inbox) = mpsc::sync_channel(8);
    if front
        .hub
        .send(HubCommand::Subscribe {
            id,
            subscription,
            frames,
            notify: front.hub.clone(),
        })
        .is_err()
    {
        return Ok(());
    }
    let mut last_write = Instant::now();
    let result = loop {
        if !tailnet_still_enabled(front, tailnet) {
            break Ok(());
        }
        let written = match inbox.recv_timeout(Duration::from_secs(1)) {
            Ok(Frame::Snapshot(json)) => write!(writer, "event: snapshot\ndata: {json}\n\n"),
            Ok(Frame::Spaces(events)) => events.iter().try_for_each(|event| match event {
                SpaceEvent::Space { key, frame } => write!(
                    writer,
                    "event: space\ndata: {{\"key\":\"{}\",\"frame\":{frame}}}\n\n",
                    key.as_str()
                ),
                SpaceEvent::Gone { key } => write!(
                    writer,
                    "event: gone\ndata: {{\"key\":\"{}\"}}\n\n",
                    key.as_str()
                ),
                SpaceEvent::Ready { keys } => {
                    let data = serde_json::json!({"keys": keys});
                    write!(writer, "event: ready\ndata: {data}\n\n")
                }
            }),
            Ok(Frame::Restarting) => {
                let _ = write!(writer, "event: restarting\ndata: {{}}\n\n");
                break writer.flush();
            }
            Err(RecvTimeoutError::Disconnected) => break Ok(()),
            Err(RecvTimeoutError::Timeout) if client_closed(&stream) => break Ok(()),
            Err(RecvTimeoutError::Timeout) if last_write.elapsed() >= PING_EVERY => {
                write!(writer, ": ping\n\n")
            }
            Err(RecvTimeoutError::Timeout) => continue,
        };
        if let Err(error) = written.and_then(|()| writer.flush()) {
            break Err(error);
        }
        last_write = Instant::now();
    };
    let _ = front.hub.send(HubCommand::Unsubscribe { id });
    result
}

/// An EventSource never sends after its request, so a readable EOF means
/// the tab went away. This notices within a second instead of at the next ping.
fn client_closed(stream: &TcpStream) -> bool {
    if stream.set_nonblocking(true).is_err() {
        return true;
    }
    let peeked = stream.peek(&mut [0u8; 1]);
    let _ = stream.set_nonblocking(false);
    match peeked {
        Ok(0) => true,
        Ok(_) => false,
        Err(error) => error.kind() != io::ErrorKind::WouldBlock,
    }
}

fn read_request(reader: &mut impl BufRead, port: u16) -> Result<Request, Status> {
    fn line(reader: &mut impl BufRead, budget: &mut u64) -> Result<String, Status> {
        let mut bytes = Vec::new();
        let count = reader
            .take(*budget + 1)
            .read_until(b'\n', &mut bytes)
            .map_err(|_| Status::BadRequest)? as u64;
        if count > *budget || !bytes.ends_with(b"\r\n") {
            return Err(Status::BadRequest);
        }
        *budget -= count;
        bytes.truncate(bytes.len() - 2);
        String::from_utf8(bytes).map_err(|_| Status::BadRequest)
    }
    let mut budget = HEADER_LIMIT;
    let first = line(reader, &mut budget)?;
    let mut parts = first.split(' ');
    let (Some(method), Some(target), Some(version)) = (parts.next(), parts.next(), parts.next())
    else {
        return Err(Status::BadRequest);
    };
    if parts.next().is_some()
        || !matches!(version, "HTTP/1.1" | "HTTP/1.0")
        || method.is_empty()
        || !target.starts_with('/')
        || target.chars().any(char::is_control)
    {
        return Err(Status::BadRequest);
    }
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_owned(), Some(query.to_owned())),
        None => (target.to_owned(), None),
    };
    let mut headers: Vec<(String, String)> = Vec::new();
    loop {
        let header = line(reader, &mut budget)?;
        if header.is_empty() {
            break;
        }
        let (name, value) = header.split_once(':').ok_or(Status::BadRequest)?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
            || value.chars().any(|ch| ch.is_control() && ch != '\t')
        {
            return Err(Status::BadRequest);
        }
        let name = name.to_ascii_lowercase();
        if matches!(name.as_str(), "transfer-encoding" | "expect")
            || headers.iter().any(|(prior, _)| prior == &name)
        {
            return Err(Status::BadRequest);
        }
        headers.push((name, value.trim().to_owned()));
    }
    if let Some((_, cookie)) = headers.iter().find(|(name, _)| name == "cookie") {
        let own_name = cookie_name(port);
        let mut seen_session = false;
        for pair in cookie.split(';') {
            let Some((name, _)) = pair.trim().split_once('=') else {
                continue;
            };
            if name == own_name && std::mem::replace(&mut seen_session, true) {
                return Err(Status::BadRequest);
            }
        }
    }
    Ok(Request {
        method: method.to_owned(),
        path,
        query,
        headers,
    })
}

fn respond_status(writer: &mut TcpStream, status: Status) -> io::Result<()> {
    respond(
        writer,
        status.line(),
        "text/plain; charset=utf-8",
        "",
        status.body().as_bytes(),
    )
}

fn respond(
    writer: &mut TcpStream,
    status: &str,
    content_type: &str,
    extra_headers: &str,
    body: &[u8],
) -> io::Result<()> {
    write!(
        writer,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n{SECURITY_HEADERS}{extra_headers}Connection: close\r\n\r\n",
        body.len()
    )?;
    writer.write_all(body)?;
    writer.flush()?;
    // Send EOF, then give an in-flight request a bounded chance to finish writing.
    writer.shutdown(Shutdown::Write)?;
    writer.set_read_timeout(Some(Duration::from_millis(50)))?;
    let deadline = Instant::now() + Duration::from_millis(250);
    let mut remaining = 2 * 1024 * 1024;
    let mut chunk = [0u8; 16 * 1024];
    while remaining > 0 && Instant::now() < deadline {
        let limit = remaining.min(chunk.len());
        match writer.read(&mut chunk[..limit]) {
            Ok(0) | Err(_) => break,
            Ok(count) => remaining -= count,
        }
    }
    Ok(())
}

#[cfg(test)]
mod history_store_tests {
    use super::*;

    #[test]
    fn history_open_failure_is_sticky_after_database_repair() {
        let state =
            std::env::temp_dir().join(format!("namedrop-history-failed-{}", std::process::id()));
        std::fs::create_dir(&state).unwrap();
        let database = state.join("namedrop.sqlite3");
        std::fs::write(&database, b"invalid sqlite database").unwrap();
        let mut cache = HistoryStore::Unopened;
        assert!(matches!(cache.store(&state), Err(Status::Unavailable)));
        std::fs::remove_file(&database).unwrap();
        drop(StateStore::open(&state).unwrap());
        assert!(StateStore::open_read_only(&state).unwrap().is_some());
        assert!(matches!(cache.store(&state), Err(Status::Unavailable)));
        drop(cache);
        std::fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn history_missing_database_can_appear_later() {
        let state =
            std::env::temp_dir().join(format!("namedrop-history-missing-{}", std::process::id()));
        let mut cache = HistoryStore::Unopened;
        assert!(matches!(cache.store(&state), Err(Status::NotFound)));
        drop(StateStore::open(&state).unwrap());
        assert!(
            cache
                .store(&state)
                .unwrap()
                .dashboard_spaces()
                .unwrap()
                .is_empty()
        );
        drop(cache);
        std::fs::remove_dir_all(state).unwrap();
    }
}

#[cfg(test)]
mod screen_regression_tests {
    use super::*;

    #[test]
    fn secret_splitters_are_removed_before_shared_and_screen_redaction() {
        let ranges = [
            (0x00ad, 0x00ad),
            (0x034f, 0x034f),
            (0x061c, 0x061c),
            (0x115f, 0x1160),
            (0x17b4, 0x17b5),
            (0x180b, 0x180f),
            (0x200b, 0x200f),
            (0x2028, 0x202e),
            (0x2060, 0x206f),
            (0x3164, 0x3164),
            (0xfe00, 0xfe0f),
            (0xfeff, 0xfeff),
            (0xffa0, 0xffa0),
            (0xfff0, 0xfffb),
            (0x1bca0, 0x1bca3),
            (0x1d173, 0x1d17a),
            (0xe0000, 0xe0fff),
        ];
        let mut failures = Vec::new();
        for (first, last) in ranges {
            for code in first..=last {
                let splitter = char::from_u32(code).unwrap();
                for raw in [
                    format!("s{splitter}k-abcdefghijklmnopqrstuvwxyz123456"),
                    format!("sk-abcdefghijklmnop{splitter}qrstuvwxyz123456"),
                ] {
                    for (surface, text) in [
                        (
                            "message",
                            snapshot::SafeText::from_untrusted(&raw, usize::MAX).text,
                        ),
                        (
                            "screen",
                            snapshot::SafeText::from_untrusted(
                                &strip_terminal_sequences(&raw),
                                usize::MAX,
                            )
                            .text,
                        ),
                    ] {
                        if text != "[REDACTED TOKEN]" && failures.len() < 20 {
                            failures.push(format!("{surface} U+{code:04X} => {text:?}"));
                        }
                    }
                }
            }
        }
        for raw in [
            "s\tk-abcdefghijklmnopqrstuvwxyz123456",
            "sk-abcdefghijklmnop\tqrstuvwxyz123456",
            "gh\tp_abcdefghijklmnopqrstuvwxyz123456",
            "sk-abcdefghijklmnop\u{1b}\nqrstuvwxyz123456",
            "s\u{1b}\nk-abcdefghijklmnopqrstuvwxyz123456",
            "s\u{1b}\u{200d}\nk-abcdefghijklmnopqrstuvwxyz123456",
            "s\u{1b}\u{fe0f}\nk-abcdefghijklmnopqrstuvwxyz123456",
            "sk-abcdefghijklmnop\u{1b}\r\nqrstuvwxyz123456",
        ] {
            for text in [
                snapshot::SafeText::from_untrusted(raw, usize::MAX).text,
                snapshot::SafeText::from_untrusted(&strip_terminal_sequences(raw), usize::MAX).text,
            ] {
                if text != "[REDACTED TOKEN]" {
                    failures.push(format!("{raw:?} => {text:?}"));
                }
            }
        }
        assert!(
            failures.is_empty(),
            "split secrets escaped redaction: {failures:#?}"
        );
    }

    #[test]
    fn sanitation_preserves_tabs_real_newlines_private_key_state_and_escape_sentinel() {
        for raw in ["clean\tline\n\tname\tvalue", "one\ntwo", "\tplain\ttext"] {
            assert_eq!(
                snapshot::SafeText::from_untrusted(raw, usize::MAX).text,
                raw
            );
            assert_eq!(strip_terminal_sequences(raw), raw);
        }
        let private = "before\n-----BE\tGIN PRIVATE KEY-----\nBODY\tPRIVATE\n-----END PRIVATE KEY-----\nafter";
        assert_eq!(
            snapshot::SafeText::from_untrusted(private, usize::MAX).text,
            "before\n[REDACTED PRIVATE KEY]\nafter"
        );
        assert_eq!(strip_terminal_sequences("a\u{1b}\nZ"), "aZ");
        assert_eq!(
            snapshot::SafeText::from_untrusted(
                "\u{2003}\ts\tk-abcdefghijklmnopqrstuvwxyz123456",
                usize::MAX
            )
            .text,
            "\u{2003}\t[REDACTED TOKEN]"
        );
        let wrapped = [
            format!("sk-proj-{}", "a".repeat(111)),
            format!("{}UNIQUE_SECRET_TAIL", "a".repeat(53)),
        ];
        let joined = wrapped.join("");
        assert_eq!(
            snapshot::SafeText::from_untrusted(&joined, usize::MAX).text,
            "[REDACTED TOKEN]"
        );
    }

    #[test]
    fn persona_input_keeps_its_existing_emoji_and_script_formatting_contract() {
        for persona in [
            "Builds UI \u{2764}\u{fe0f} and \u{1f469}\u{200d}\u{1f4bb}",
            "Mongolian variation \u{180b}",
        ] {
            assert_eq!(crate::domain::validate_persona(persona), Ok(()));
        }
    }

    #[test]
    fn orphan_follower_has_a_deadline_without_publishing_or_evicting() {
        let reads = Arc::new(ScreenReads::default());
        let target = ScreenTarget {
            socket: "orphan-socket".into(),
            pane: "orphan-pane".into(),
        };
        let flight = Arc::new(ScreenFlight::default());
        reads
            .flights
            .lock()
            .unwrap()
            .insert(target.clone(), flight.clone());
        let (sender, receiver) = mpsc::channel();
        let started = Instant::now();
        let follower = {
            let reads = reads.clone();
            let target = target.clone();
            thread::spawn(move || sender.send(reads.read(target)).unwrap())
        };
        let outcome = receiver.recv_timeout(Duration::from_secs(5));
        let elapsed = started.elapsed();
        let pending = flight.result.lock().unwrap().is_none();
        let retained = reads
            .flights
            .lock()
            .unwrap()
            .get(&target)
            .is_some_and(|entry| Arc::ptr_eq(entry, &flight));
        *flight.result.lock().unwrap() = Some(Ok(Arc::from("cleanup")));
        flight.ready.notify_all();
        follower.join().unwrap();
        assert!(
            matches!(outcome, Ok(Err(Status::Unavailable))),
            "orphan follower did not return 503 within 5 seconds: {outcome:?}"
        );
        assert!(elapsed >= Duration::from_secs(3) && elapsed < Duration::from_secs(5));
        assert!(
            pending && retained,
            "timed out follower changed the leader's flight"
        );
    }
}

#[cfg(test)]
mod skill_route_tests {
    use super::*;

    #[test]
    fn skills_require_session_provenance_and_safe_member_path() {
        let state =
            std::env::temp_dir().join(format!("namedrop-skill-route-{}", std::process::id()));
        let files = ServerFiles::at(&state).unwrap();
        let token = files.token().unwrap();
        let session = files.session().unwrap();
        let mut request = Request {
            method: "GET".into(),
            path: "/w/0123456789abcdef0123/participants/member/skills".into(),
            query: None,
            headers: vec![("host".into(), "127.0.0.1:1234".into())],
        };
        let routes = |request: &Request| route(request, 1234, &token, &session, false, None);
        assert!(matches!(routes(&request), Err(Status::Unauthorized)));
        request.headers.push((
            "cookie".into(),
            format!("{}={}", cookie_name(1234), session.as_str()),
        ));
        request
            .headers
            .push(("origin".into(), "https://other.invalid".into()));
        assert!(matches!(routes(&request), Err(Status::Forbidden)));
        request.headers.pop();
        request
            .headers
            .push(("sec-fetch-site".into(), "same-origin".into()));
        assert!(
            matches!(routes(&request), Ok(Route::Skills { address_id, .. }) if address_id == "member")
        );
        request.path = "/w/0123456789abcdef0123/participants/bad%2Fmember/skills".into();
        assert!(matches!(routes(&request), Err(Status::NotFound)));
        request.path = "/w/invalid-key/participants/member/skills".into();
        assert!(matches!(routes(&request), Err(Status::NotFound)));
        request.path = "/w/0123456789abcdef0123/participants/member/skills".into();
        request.method = "POST".into();
        assert!(matches!(routes(&request), Err(Status::MethodNotAllowed)));
        std::fs::remove_dir_all(state).unwrap();
    }
}

#[cfg(test)]
mod reminder_route_tests {
    use super::*;

    #[test]
    fn reminders_require_session_provenance_and_csrf_for_cancel() {
        let state = std::env::temp_dir().join(crate::IdGenerator::next_id(
            &crate::ProcessIds::default(),
            "reminder-route",
        ));
        let files = ServerFiles::at(&state).unwrap();
        let token = files.token().unwrap();
        let session = files.session().unwrap();
        for scope in ["threads/thread", "direct-messages/agent"] {
            let path = format!("/w/0123456789abcdef0123/{scope}/reminders");
            let mut request = Request {
                method: "GET".into(),
                path: path.clone(),
                query: None,
                headers: vec![("host".into(), "127.0.0.1:1234".into())],
            };
            let routes = |request: &Request| route(request, 1234, &token, &session, false, None);
            assert!(matches!(routes(&request), Err(Status::Unauthorized)));
            request.headers.push((
                "cookie".into(),
                format!("{}={}", cookie_name(1234), session.as_str()),
            ));
            request
                .headers
                .push(("origin".into(), "https://other.invalid".into()));
            assert!(matches!(routes(&request), Err(Status::Forbidden)));
            request.headers.pop();
            request
                .headers
                .push(("sec-fetch-site".into(), "same-origin".into()));
            assert!(matches!(
                routes(&request),
                Ok(Route::Reminders { cancel: None, .. })
            ));
            request.method = "POST".into();
            request.path = format!("{path}/reminder/cancel");
            assert!(matches!(routes(&request), Err(Status::Forbidden)));
            request
                .headers
                .push(("x-namedrop-csrf".into(), csrf_token(&session)));
            assert!(
                matches!(routes(&request), Ok(Route::Reminders { cancel: Some(id), .. }) if id == "reminder")
            );
            request
                .headers
                .push(("origin".into(), "https://other.invalid".into()));
            assert!(matches!(routes(&request), Err(Status::Forbidden)));
            request.headers.pop();
            request.path = format!("{path}/bad%2Fid/cancel");
            assert!(matches!(routes(&request), Err(Status::NotFound)));
            request.path = format!("{path}/reminder/cancel");
            request.method = "GET".into();
            assert!(matches!(routes(&request), Err(Status::MethodNotAllowed)));
        }
        std::fs::remove_dir_all(state).unwrap();
    }
}

#[cfg(test)]
mod tailnet_route_tests {
    use super::super::tailscale::OwnedHandler;
    use super::*;

    fn handler(port: u16) -> OwnedHandler {
        OwnedHandler {
            dns_name: "device.tail123.ts.net".into(),
            https_port: port,
            owner_login: "owner@example.com".into(),
            backend_port: 1234,
        }
    }

    fn request(host: &str, login: Option<&str>, origin: Option<&str>) -> Request {
        let mut headers = vec![("host".into(), host.into())];
        if let Some(login) = login {
            headers.push(("tailscale-user-login".into(), login.into()));
        }
        if let Some(origin) = origin {
            headers.push(("origin".into(), origin.into()));
        }
        Request {
            method: "GET".into(),
            path: "/api/session".into(),
            query: None,
            headers,
        }
    }

    #[test]
    fn exact_tailnet_authority_identity_and_optional_https_origin_matrix() {
        for https_port in [443, 8443] {
            let handler = handler(https_port);
            let authority = handler.authority();
            let origin = handler.origin();
            for host in [
                authority.clone(),
                "untrusted.example".into(),
                format!("{authority}.evil"),
                format!("{authority}."),
                format!("{authority}:443"),
            ] {
                for login in [
                    None,
                    Some("owner@example.com"),
                    Some("other@example.com"),
                    Some(""),
                ] {
                    for supplied in [
                        None,
                        Some(origin.as_str()),
                        Some("https://evil.example"),
                        Some("http://device.tail123.ts.net"),
                        Some("null"),
                    ] {
                        let request = request(&host, login, supplied);
                        let accepted = admit_host(&request, 1234, Some(&handler)).is_ok();
                        assert_eq!(
                            accepted,
                            host == authority
                                && login == Some("owner@example.com")
                                && supplied.is_none_or(|value| value == origin),
                            "host={host} login={login:?} origin={supplied:?}"
                        );
                        assert!(admit_host(&request, 1234, None).is_err());
                    }
                }
            }
            assert!(
                admit_host(
                    &request(&authority, Some("owner@example.com"), None),
                    9999,
                    Some(&handler)
                )
                .is_err()
            );
        }
    }

    #[test]
    fn proxy_cannot_disguise_itself_as_local_even_when_tailnet_is_off() {
        let handler = handler(443);
        for host in ["127.0.0.1:1234", "localhost:1234"] {
            let plain = request(host, None, None);
            assert!(admit_host(&plain, 1234, None).is_ok());
            assert!(admit_host(&plain, 1234, Some(&handler)).is_ok());
            for marker in [
                "x-forwarded-host",
                "x-forwarded-for",
                "x-forwarded-proto",
                "tailscale-user-login",
                "tailscale-funnel-request",
                "forwarded",
            ] {
                for value in ["", "other@example.com", "owner@example.com"] {
                    let mut proxied = request(host, None, None);
                    proxied.headers.push((marker.into(), value.into()));
                    assert!(admit_host(&proxied, 1234, None).is_err(), "{marker}");
                    assert!(
                        admit_host(&proxied, 1234, Some(&handler)).is_err(),
                        "{marker}"
                    );
                }
            }
        }
        for host in [
            "localhost",
            "127.0.0.1",
            "127.0.0.2:1234",
            "localhost:4321",
            "[::1]:1234",
        ] {
            assert!(admit_host(&request(host, None, None), 1234, Some(&handler)).is_err());
        }
    }

    #[test]
    fn every_tailnet_route_checks_identity_before_assets_health_redemption_or_session() {
        let state = std::env::temp_dir().join(format!(
            "namedrop-tailnet-routes-{}",
            super::super::server_files::random_hex(12).unwrap()
        ));
        let files = ServerFiles::at(&state).unwrap();
        let token = files.token().unwrap();
        let session = files.session().unwrap();
        let handler = handler(443);
        let paths = [
            "/",
            "/favicon.ico",
            "/healthz",
            "/api/session",
            "/api/events",
            "/api/spaces",
            "/api/direct-messages",
            "/api/images/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.png",
            "/w/0123456789abcdef0123",
            "/w/0123456789abcdef0123/events",
            "/w/0123456789abcdef0123/participants/agent/live",
            "/w/0123456789abcdef0123/participants/agent/skills",
            "/w/0123456789abcdef0123/threads/thread/reminders",
        ];
        for path in paths {
            let mut request = request(
                &handler.authority(),
                Some("owner@example.com"),
                Some(&handler.origin()),
            );
            request.path = path.into();
            request.headers.push((
                "cookie".into(),
                format!("{}={}", cookie_name(1234), session.as_str()),
            ));
            request.query = (path == "/healthz").then(|| format!("t={}", token.as_str()));
            assert!(
                route(&request, 1234, &token, &session, false, Some(&handler)).is_ok(),
                "{path}"
            );
            assert!(
                matches!(
                    route(&request, 1234, &token, &session, false, None),
                    Err(Status::Forbidden)
                ),
                "{path}"
            );
            request
                .headers
                .iter_mut()
                .find(|(name, _)| name == "tailscale-user-login")
                .unwrap()
                .1 = "wrong@example.com".into();
            assert!(
                matches!(
                    route(&request, 1234, &token, &session, false, Some(&handler)),
                    Err(Status::Forbidden)
                ),
                "{path}"
            );
        }
        let mut request = request(
            &handler.authority(),
            Some("owner@example.com"),
            Some(&handler.origin()),
        );
        request.path = "/".into();
        request.query = Some("n=one-time-entry".into());
        assert!(matches!(
            route(&request, 1234, &token, &session, false, Some(&handler)),
            Ok(Route::Redeem { .. })
        ));
        request
            .headers
            .push(("sec-fetch-site".into(), "cross-site".into()));
        assert!(matches!(
            route(&request, 1234, &token, &session, false, Some(&handler)),
            Err(Status::Forbidden)
        ));
        std::fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn csrf_session_and_fetch_metadata_still_gate_https_mutations() {
        let state = std::env::temp_dir().join(format!(
            "namedrop-tailnet-csrf-{}",
            super::super::server_files::random_hex(12).unwrap()
        ));
        let files = ServerFiles::at(&state).unwrap();
        let token = files.token().unwrap();
        let session = files.session().unwrap();
        let handler = handler(8443);
        for (host, login, origin, policy) in [
            (
                handler.authority(),
                Some("owner@example.com"),
                handler.origin(),
                Some(&handler),
            ),
            (
                "127.0.0.1:1234".into(),
                None,
                "http://127.0.0.1:1234".into(),
                None,
            ),
        ] {
            for path in ["/w/0123456789abcdef0123/messages", "/api/images"] {
                let mut request = request(&host, login, Some(&origin));
                request.method = "POST".into();
                request.path = path.into();
                assert!(matches!(
                    route(&request, 1234, &token, &session, false, policy),
                    Err(Status::Unauthorized)
                ));
                request.headers.push((
                    "cookie".into(),
                    format!("{}={}", cookie_name(1234), session.as_str()),
                ));
                assert!(matches!(
                    route(&request, 1234, &token, &session, false, policy),
                    Err(Status::Forbidden)
                ));
                request
                    .headers
                    .push(("x-namedrop-csrf".into(), csrf_token(&session)));
                let routed = route(&request, 1234, &token, &session, false, policy);
                assert!(
                    if path == "/api/images" {
                        matches!(routed, Ok(Route::ImageUpload))
                    } else {
                        matches!(routed, Ok(Route::Send(_)))
                    },
                    "{path}"
                );
                request
                    .headers
                    .push(("sec-fetch-site".into(), "same-site".into()));
                assert!(matches!(
                    route(&request, 1234, &token, &session, false, policy),
                    Err(Status::Forbidden)
                ));
                request.headers.pop();
                request
                    .headers
                    .iter_mut()
                    .find(|(name, _)| name == "origin")
                    .unwrap()
                    .1 = "https://evil.example".into();
                assert!(matches!(
                    route(&request, 1234, &token, &session, false, policy),
                    Err(Status::Forbidden)
                ));
            }
        }
        std::fs::remove_dir_all(state).unwrap();
    }
}
