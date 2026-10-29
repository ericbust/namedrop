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
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, RecvTimeoutError, Sender},
    },
    thread,
    time::{Duration, Instant},
};

const INDEX_HTML: &str = include_str!("assets/index.html");
const APP_JS: &str = include_str!("assets/app.js");
const APP_CSS: &str = include_str!("assets/app.css");
const BODY_LIMIT: usize = 64 * 1024;
const HEADER_LIMIT: u64 = 8 * 1024;
const PING_EVERY: Duration = Duration::from_secs(15);
const MAX_CONNECTIONS: usize = 64;
// Followers occupy HTTP handlers too. Keep 48 connection slots for other routes.
const MAX_SCREEN_REQUESTS: usize = 16;
const SECURITY_HEADERS: &str = "Content-Security-Policy: default-src 'none'; script-src 'self'; \
style-src 'self'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; \
base-uri 'none'; form-action 'none'\r\nX-Frame-Options: DENY\r\n\
X-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nCache-Control: no-store\r\n";

pub(crate) struct Front {
    pub port: u16,
    pub files: ServerFiles,
    pub token: Token,
    pub session: Token,
    pub instance: String,
    pub hub: Sender<HubCommand>,
    pub assets: Assets,
    pub restarting: Arc<AtomicBool>,
}

pub(crate) fn accept_loop(listener: TcpListener, front: Arc<Front>, stop: Arc<AtomicBool>) {
    let next_id = Arc::new(AtomicU64::new(1));
    let open = Arc::new(AtomicUsize::new(0));
    let history_store = Arc::new(Mutex::new(HistoryStore::Unopened));
    let screens = Arc::new(ScreenReads::default());
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
        thread::spawn(move || {
            let _permit = permit;
            let _ = serve_connection(stream, &front, &next_id, &history_store, &screens);
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

fn strip_terminal_sequences(raw: &str) -> String {
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
    BadRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    Gone,
    MethodNotAllowed,
    Unavailable,
    PayloadTooLarge,
}

impl Status {
    fn line(self) -> &'static str {
        match self {
            Self::PayloadTooLarge => "413 Payload Too Large",
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
            Self::Unauthorized => {
                "This dashboard needs its key. Run `namedrop dashboard` in that workspace to open it.\n"
            }
            Self::PayloadTooLarge => "Request body is too large.\n",
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
                let bytes = match path {
                    "/index.html" => INDEX_HTML.as_bytes(),
                    "/assets/app.js" => APP_JS.as_bytes(),
                    "/assets/app.css" => APP_CSS.as_bytes(),
                    _ => return None,
                };
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
    Redeem {
        location: String,
        nonce: String,
    },
    Page,
    Session,
    Send(SpaceKey),
    Screen {
        key: SpaceKey,
        address_id: String,
    },
    Events(SpaceKey),
    Messages {
        key: SpaceKey,
        before: Option<String>,
        limit: i64,
    },
    AllEvents,
    Spaces,
    Asset(String),
    Favicon,
    Health,
}

fn route(
    request: &Request,
    port: u16,
    token: &Token,
    session: &Token,
    directory: bool,
) -> Result<Route, Status> {
    let host = request.header("host").ok_or(Status::Forbidden)?;
    // Exact loopback Host defeats DNS rebinding onto this port.
    if host != format!("127.0.0.1:{port}") && host != format!("localhost:{port}") {
        return Err(Status::Forbidden);
    }
    if request.header("sec-fetch-site") == Some("cross-site") {
        return Err(Status::Forbidden);
    }
    if request.method != "GET" && request.method != "POST" {
        return Err(Status::MethodNotAllowed);
    }
    let path = request.path.as_str();
    if request.method == "POST" {
        authenticate_session(request, port, session)?;
        authenticate_mutation(request, session, host)?;
        let key = path
            .strip_prefix("/w/")
            .and_then(|rest| rest.strip_suffix("/messages"))
            .and_then(SpaceKey::parse)
            .ok_or(Status::NotFound)?;
        return Ok(Route::Send(key));
    }
    if let Some(rest) = path.strip_prefix("/w/") {
        let segments: Vec<_> = rest.split('/').collect();
        if segments.get(1) == Some(&"participants") {
            authenticate_session(request, port, session)?;
            authenticate_provenance(request, host)?;
            let [key, "participants", address, "screen"] = segments.as_slice() else {
                return Err(Status::NotFound);
            };
            let key = SpaceKey::parse(key).ok_or(Status::NotFound)?;
            let address_id = decode_asset_path(address).ok_or(Status::NotFound)?;
            if address_id.is_empty() || address_id.contains('/') {
                return Err(Status::NotFound);
            }
            return Ok(Route::Screen { key, address_id });
        }
    }
    match path {
        "/healthz" => {
            return match request.query("t") {
                Some(candidate) if token.matches(candidate) => Ok(Route::Health),
                _ => Err(Status::Unauthorized),
            };
        }
        "/assets/app.js" => return Ok(Route::Asset(path.to_owned())),
        "/assets/app.css" => return Ok(Route::Asset(path.to_owned())),
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
        return Ok(Route::Redeem {
            location: path.to_owned(),
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

fn authenticate_mutation(request: &Request, session: &Token, host: &str) -> Result<(), Status> {
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
    authenticate_provenance(request, host)
}

fn authenticate_provenance(request: &Request, host: &str) -> Result<(), Status> {
    if request
        .header("origin")
        .is_some_and(|origin| origin != format!("http://{host}"))
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
        {
            return Err(Status::BadRequest);
        }
        Ok(post)
    }
}

/// Cookies ignore ports, so the name carries it to keep two servers apart.
fn cookie_name(port: u16) -> String {
    format!("namedrop_dashboard_{port}")
}

fn serve_connection(
    stream: TcpStream,
    front: &Front,
    next_id: &AtomicU64,
    history_store: &Mutex<HistoryStore>,
    screens: &ScreenReads,
) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(&stream);
    let request = match read_request(&mut reader, front.port) {
        Ok(request) => request,
        Err(status) => return respond_status(&mut writer, status),
    };
    match route(
        &request,
        front.port,
        &front.token,
        &front.session,
        matches!(front.assets, Assets::Directory(_)),
    ) {
        Err(status) => respond_status(&mut writer, status),
        Ok(Route::Redeem { nonce, .. }) if !front.files.consume_entry(&nonce) => {
            respond_status(&mut writer, Status::Unauthorized)
        }
        Ok(Route::Redeem { location, .. }) => {
            let headers = format!(
                "Location: {location}\r\nSet-Cookie: {}={}; HttpOnly; SameSite=Strict; Path=/\r\n",
                cookie_name(front.port),
                front.session.as_str()
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
        Ok(Route::Send(key)) => {
            let post = match OperatorPost::read(&mut reader, &request) {
                Ok(post) => post,
                Err(status) => return respond_status(&mut writer, status),
            };
            send_operator_post(&mut writer, front, key, post)
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
        Ok(Route::Page) => serve_asset(&mut writer, front, &request, "/index.html"),
        Ok(Route::Asset(path)) => serve_asset(&mut writer, front, &request, &path),
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
        Ok(Route::Events(key)) => {
            stream_events(stream, writer, front, next_id, Subscription::Space(key))
        }
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
        Ok(Route::AllEvents) => stream_events(stream, writer, front, next_id, Subscription::All),
    }
}

fn send_operator_post(
    writer: &mut TcpStream,
    front: &Front,
    key: SpaceKey,
    post: OperatorPost,
) -> io::Result<()> {
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
    let sent = if crate::message::is_plain_channel_post(&post.source) {
        crate::message::send_operator_channel_message(
            &post.request_id,
            &post.source,
            target.as_ref(),
            &runtime,
        )
    } else {
        crate::send_operator_message(&post.request_id, &post.source, target.as_ref(), &runtime)
    };
    match sent {
        Ok(mut receipt) => {
            snapshot::channel_routing::finish_post(front.files.state(), &namespace, &mut receipt);
            respond(
                writer,
                "200 OK",
                "application/json",
                "",
                receipt.to_string().as_bytes(),
            )
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

fn serve_asset(
    writer: &mut TcpStream,
    front: &Front,
    request: &Request,
    path: &str,
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
        "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\n{SECURITY_HEADERS}{}Connection: close\r\n\r\n",
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
) -> io::Result<()> {
    write!(
        writer,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n{SECURITY_HEADERS}Connection: close\r\n\r\nretry: 1000\n\n"
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
