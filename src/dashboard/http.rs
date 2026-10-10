//! A std-only HTTP/1.1 front: GET only, loopback only, one thread per
//! connection. Every security check lives in `route`.
use super::hub::{Frame, HubCommand};
use super::server_files::Token;
use super::snapshot::SpaceKey;
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
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
/// Each connection is a thread; past this, new ones get 503 instead of another.
const MAX_CONNECTIONS: usize = 64;
const SECURITY_HEADERS: &str = "Content-Security-Policy: default-src 'none'; script-src 'self'; \
style-src 'self'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; \
base-uri 'none'; form-action 'none'\r\nX-Frame-Options: DENY\r\n\
X-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nCache-Control: no-store\r\n";

pub(crate) struct Front {
    pub port: u16,
    pub token: Token,
    pub instance: String,
    pub hub: Sender<HubCommand>,
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
        let slot = Slot(open.clone());
        let front = front.clone();
        let next_id = next_id.clone();
        thread::spawn(move || {
            let _slot = slot;
            let _ = serve_connection(stream, &front, &next_id);
        });
    }
}

/// Frees its connection's place in the cap however the thread ends.
struct Slot(Arc<AtomicUsize>);

impl Drop for Slot {
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

    fn query_token(&self) -> Option<&str> {
        self.query
            .as_deref()?
            .split('&')
            .find_map(|pair| pair.strip_prefix("t="))
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

enum Asset {
    Script,
    Style,
}

enum Route {
    /// Swap the one-time `?t=` for a cookie, then land on the clean URL.
    Enter(String),
    Page,
    Events(SpaceKey),
    Spaces,
    Asset(Asset),
    Favicon,
    Health,
}

fn route(request: &Request, port: u16, token: &Token) -> Result<Route, Status> {
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
            return match request.query_token() {
                Some(candidate) if token.matches(candidate) => Ok(Route::Health),
                _ => Err(Status::Unauthorized),
            };
        }
        "/assets/app.js" => return Ok(Route::Asset(Asset::Script)),
        "/assets/app.css" => return Ok(Route::Asset(Asset::Style)),
        "/favicon.ico" => return Ok(Route::Favicon),
        _ => {}
    }
    let page = match path {
        "/" | "/api/spaces" => None,
        _ => {
            let rest = path.strip_prefix("/w/").ok_or(Status::NotFound)?;
            let (segment, events) = match rest.strip_suffix("/events") {
                Some(segment) => (segment, true),
                None => (rest, false),
            };
            Some((SpaceKey::parse(segment).ok_or(Status::NotFound)?, events))
        }
    };
    if let Some(candidate) = request.query_token() {
        return if token.matches(candidate) {
            Ok(Route::Enter(path.to_owned()))
        } else {
            Err(Status::Unauthorized)
        };
    }
    if !request
        .cookie(&cookie_name(port))
        .is_some_and(|candidate| token.matches(candidate))
    {
        return Err(Status::Unauthorized);
    }
    Ok(match (path, page) {
        ("/api/spaces", _) => Route::Spaces,
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
    match route(&request, front.port, &front.token) {
        Err(status) => respond_status(&mut writer, status),
        Ok(Route::Enter(location)) => {
            let headers = format!(
                "Location: {location}\r\nSet-Cookie: {}={}; HttpOnly; SameSite=Strict; Path=/\r\n",
                cookie_name(front.port),
                front.token.as_str()
            );
            respond(&mut writer, "303 See Other", "text/plain", &headers, b"")
        }
        Ok(Route::Page) => respond(
            &mut writer,
            "200 OK",
            "text/html; charset=utf-8",
            "",
            INDEX_HTML.as_bytes(),
        ),
        Ok(Route::Asset(Asset::Script)) => respond(
            &mut writer,
            "200 OK",
            "text/javascript; charset=utf-8",
            "",
            APP_JS.as_bytes(),
        ),
        Ok(Route::Asset(Asset::Style)) => respond(
            &mut writer,
            "200 OK",
            "text/css; charset=utf-8",
            "",
            APP_CSS.as_bytes(),
        ),
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
        Ok(Route::Events(key)) => stream_events(stream, writer, front, next_id, key),
    }
}

fn stream_events(
    stream: TcpStream,
    mut writer: TcpStream,
    front: &Front,
    next_id: &AtomicU64,
    key: SpaceKey,
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
        .send(HubCommand::Subscribe { id, key, frames })
        .is_err()
    {
        return Ok(());
    }
    let mut last_write = Instant::now();
    let result = loop {
        let written = match inbox.recv_timeout(Duration::from_secs(1)) {
            Ok(Frame::Snapshot(json)) => write!(writer, "event: snapshot\ndata: {json}\n\n"),
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
