use super::hub::{Frame, HubCommand, SpaceEvent, Subscription};
use super::server_files::{ServerFiles, Token, UiSource};
use super::snapshot::SpaceKey;
use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::OpenOptionsExt,
    },
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, RecvTimeoutError, Sender},
    },
    thread,
    time::{Duration, Instant},
};

const INDEX_HTML: &str = include_str!("assets/index.html");
const APP_JS: &str = include_str!("assets/app.js");
const APP_CSS: &str = include_str!("assets/app.css");
const HEADER_LIMIT: u64 = 8 * 1024;
const PING_EVERY: Duration = Duration::from_secs(15);
const MAX_CONNECTIONS: usize = 64;
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
}

pub(crate) fn accept_loop(listener: TcpListener, front: Arc<Front>) {
    let next_id = Arc::new(AtomicU64::new(1));
    let open = Arc::new(AtomicUsize::new(0));
    for mut stream in listener.incoming().flatten() {
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
        thread::spawn(move || {
            let _permit = permit;
            let _ = serve_connection(stream, &front, &next_id);
        });
    }
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
    MethodNotAllowed,
}

impl Status {
    fn line(self) -> &'static str {
        match self {
            Self::BadRequest => "400 Bad Request",
            Self::Unauthorized => "401 Unauthorized",
            Self::Forbidden => "403 Forbidden",
            Self::NotFound => "404 Not Found",
            Self::MethodNotAllowed => "405 Method Not Allowed",
        }
    }

    fn body(self) -> &'static str {
        match self {
            Self::Unauthorized => {
                "This dashboard needs its key. Run `namedrop dashboard` in that workspace to open it.\n"
            }
            Self::BadRequest => "Bad request.\n",
            Self::Forbidden => "Forbidden.\n",
            Self::NotFound => "Not found.\n",
            Self::MethodNotAllowed => "The dashboard is read-only.\n",
        }
    }
}

pub(crate) enum Assets {
    Embedded,
    Directory(File),
}

impl Assets {
    pub fn new(source: &UiSource) -> Result<Self, String> {
        match source {
            UiSource::Embedded => Ok(Self::Embedded),
            UiSource::Directory(path) => OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(path)
                .map(Self::Directory)
                .map_err(|error| format!("dashboard UI directory {}: {error}", path.display())),
        }
    }

    fn read(&self, path: &str) -> Option<Vec<u8>> {
        match self {
            Self::Embedded => Some(match path {
                "/index.html" => INDEX_HTML.as_bytes().to_vec(),
                "/assets/app.js" => APP_JS.as_bytes().to_vec(),
                "/assets/app.css" => APP_CSS.as_bytes().to_vec(),
                _ => return None,
            }),
            Self::Directory(root) => {
                let parts: Vec<_> = path.strip_prefix('/')?.split('/').collect();
                if parts
                    .iter()
                    .any(|part| part.is_empty() || *part == "." || *part == "..")
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
                    // Every lookup is relative to a held directory descriptor, and no
                    // component may follow a symlink, even while files are replaced.
                    let fd = unsafe { libc::openat(file.as_raw_fd(), name.as_ptr(), flags) };
                    if fd < 0 {
                        return None;
                    }
                    // openat returned a new owned descriptor.
                    file = unsafe { File::from_raw_fd(fd) };
                }
                if !file.metadata().ok()?.is_file() {
                    return None;
                }
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes).ok()?;
                Some(bytes)
            }
        }
    }
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

fn content_type(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("json" | "map") => "application/json",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

enum Route {
    Redeem { location: String, nonce: String },
    Page,
    Events(SpaceKey),
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
    if request.method != "GET" {
        return Err(Status::MethodNotAllowed);
    }
    let path = request.path.as_str();
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
        "/" | "/api/spaces" | "/api/events" => None,
        _ if custom_index => None,
        _ => {
            let rest = path.strip_prefix("/w/").ok_or(Status::NotFound)?;
            let (segment, events) = match rest.strip_suffix("/events") {
                Some(segment) => (segment, true),
                None => (rest, false),
            };
            Some((SpaceKey::parse(segment).ok_or(Status::NotFound)?, events))
        }
    };
    if let Some(nonce) = request.query("n") {
        return Ok(Route::Redeem {
            location: path.to_owned(),
            nonce: nonce.to_owned(),
        });
    }
    if !request
        .cookie(&cookie_name(port))
        .is_some_and(|candidate| session.matches(candidate))
    {
        return Err(Status::Unauthorized);
    }
    Ok(match (path, page) {
        ("/api/spaces", _) => Route::Spaces,
        ("/api/events", _) => Route::AllEvents,
        (_, Some((key, true))) => Route::Events(key),
        _ => Route::Page,
    })
}

/// Cookies ignore ports, so the name carries it to keep two servers apart.
fn cookie_name(port: u16) -> String {
    format!("namedrop_dashboard_{port}")
}

fn serve_connection(stream: TcpStream, front: &Front, next_id: &AtomicU64) -> io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut writer = stream.try_clone()?;
    let request = match read_request(&stream) {
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
        Ok(Route::Page) => serve_asset(&mut writer, &front.assets, "/index.html"),
        Ok(Route::Asset(path)) => serve_asset(&mut writer, &front.assets, &path),
        Ok(Route::Favicon) => respond(&mut writer, "204 No Content", "text/plain", "", b""),
        Ok(Route::Health) => respond(
            &mut writer,
            "200 OK",
            "application/json",
            "",
            serde_json::json!({"instance": front.instance})
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
        Ok(Route::AllEvents) => stream_events(stream, writer, front, next_id, Subscription::All),
    }
}

fn serve_asset(writer: &mut TcpStream, assets: &Assets, path: &str) -> io::Result<()> {
    match assets.read(path) {
        Some(body) => respond(writer, "200 OK", content_type(path), "", &body),
        None => respond_status(writer, Status::NotFound),
    }
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

fn read_request(stream: &TcpStream) -> Result<Request, Status> {
    let mut reader = BufReader::new(stream.take(HEADER_LIMIT));
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|_| Status::BadRequest)?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target), Some(_version)) = (parts.next(), parts.next(), parts.next())
    else {
        return Err(Status::BadRequest);
    };
    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_owned(), Some(query.to_owned())),
        None => (target.to_owned(), None),
    };
    let method = method.to_owned();
    let mut headers = Vec::new();
    loop {
        line.clear();
        if reader
            .read_line(&mut line)
            .map_err(|_| Status::BadRequest)?
            == 0
        {
            return Err(Status::BadRequest);
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break;
        }
        let (name, value) = header.split_once(':').ok_or(Status::BadRequest)?;
        headers.push((name.trim().to_owned(), value.trim().to_owned()));
    }
    Ok(Request {
        method,
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
    writer.flush()
}
