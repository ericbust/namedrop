//! The dashboard renders untrusted collaborator text, and the server sends
//! `script-src 'self'; style-src 'self'`. These assertions keep the embedded
//! assets inside that boundary: no HTML-parsing DOM sinks, no dynamic code,
//! no inline script or style, and nothing loaded from the network.

const INDEX_HTML: &str = include_str!("../src/dashboard/assets/index.html");
const APP_JS: &str = include_str!("../src/dashboard/assets/app.js");
const APP_CSS: &str = include_str!("../src/dashboard/assets/app.css");

const ASSETS: [(&str, &str); 3] = [
    ("index.html", INDEX_HTML),
    ("app.js", APP_JS),
    ("app.css", APP_CSS),
];

/// createElementNS needs the SVG namespace name. It is an identifier, never fetched.
const SVG_NAMESPACE: &str = "http://www.w3.org/2000/svg";

fn offenders(source: &str, needles: &[&str]) -> Vec<String> {
    needles
        .iter()
        .filter(|needle| source.contains(*needle))
        .map(|needle| needle.to_string())
        .collect()
}

#[test]
fn script_never_uses_html_parsing_sinks_or_dynamic_code() {
    let forbidden = [
        "innerHTML",
        "outerHTML",
        "insertAdjacentHTML",
        "document.write",
        "eval(",
        "new Function",
        "setAttribute('style'",
        "setAttribute(\"style\"",
        "createContextualFragment",
        "DOMParser",
        "srcdoc",
    ];
    assert_eq!(
        offenders(APP_JS, &forbidden),
        Vec::<String>::new(),
        "app.js must build DOM through el()/svg() with textContent only"
    );
}

#[test]
fn string_timers_are_not_used_as_eval() {
    for timer in ["setTimeout(", "setInterval("] {
        for (index, _) in APP_JS.match_indices(timer) {
            let rest = APP_JS[index + timer.len()..].trim_start();
            assert!(
                !rest.starts_with('\'') && !rest.starts_with('"') && !rest.starts_with('`'),
                "{timer} at byte {index} is passed a string, which evaluates code"
            );
        }
    }
}

#[test]
fn no_asset_loads_anything_from_the_network() {
    for (name, source) in ASSETS {
        let scrubbed = source.replace(SVG_NAMESPACE, "");
        assert_eq!(
            offenders(&scrubbed, &["http://", "https://", "//cdn", "@import"]),
            Vec::<String>::new(),
            "{name} references a remote URL"
        );
    }
}

#[test]
fn index_page_has_no_inline_script_or_style() {
    assert_eq!(
        offenders(
            INDEX_HTML,
            &["<style", " style=", " onload=", " onclick=", "javascript:"]
        ),
        Vec::<String>::new()
    );
    let scripts: Vec<&str> = INDEX_HTML
        .match_indices("<script")
        .map(|(i, _)| &INDEX_HTML[i..])
        .collect();
    assert_eq!(scripts.len(), 1, "exactly one script tag");
    assert!(
        scripts[0].starts_with("<script src=\"/assets/app.js\""),
        "the only script is the external app.js"
    );
    assert!(INDEX_HTML.contains("<link rel=\"stylesheet\" href=\"/assets/app.css\">"));
}

#[test]
fn stylesheet_uses_system_fonts_only() {
    assert_eq!(
        offenders(APP_CSS, &["@font-face", "url("]),
        Vec::<String>::new()
    );
}

mod support;

use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant},
};
use support::TestDirectory;

struct Dashboard {
    directory: TestDirectory,
}

impl Dashboard {
    fn new() -> Self {
        let directory = TestDirectory::new();
        let herdr = directory.path().join("herdr");
        fs::write(
            &herdr,
            "#!/bin/sh\nprintf '{\"result\":{\"workspace\":{\"label\":\"test\"}}}'\n",
        )
        .unwrap();
        fs::set_permissions(herdr, fs::Permissions::from_mode(0o755)).unwrap();
        Self { directory }
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn state(&self) -> PathBuf {
        self.root().join("state")
    }

    fn ui(&self, name: &str) -> PathBuf {
        let path = self.root().join(name);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("index.html"), format!("<h1>{name}</h1>")).unwrap();
        path
    }

    fn command(&self, ui: Option<&Path>) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_namedrop"));
        command
            .current_dir(self.root())
            .args(["--json", "--plugin-state"])
            .arg(self.state())
            .args([
                "--herdr-socket",
                "assets-test",
                "--herdr-workspace",
                "w1",
                "dashboard",
                "--no-open",
            ])
            .env_remove("HERDR_PANE_ID")
            .env_remove("HERDR_WORKSPACE_ID")
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_PLUGIN_STATE_DIR")
            .env("HERDR_BIN_PATH", self.root().join("herdr"))
            .env("NAMEDROP_DASHBOARD_IDLE_MS", "10000");
        if let Some(path) = ui {
            command.arg("--ui").arg(path);
        }
        command
    }

    fn open(&self, ui: Option<&Path>) -> Value {
        data(self.command(ui).output().unwrap())
    }

    fn info(&self) -> Value {
        serde_json::from_slice(&fs::read(self.state().join("dashboard/server.json")).unwrap())
            .unwrap()
    }

    fn port(&self) -> u16 {
        self.info()["port"].as_u64().unwrap() as u16
    }

    fn cookie(&self) -> String {
        format!(
            "namedrop_dashboard_{}={}",
            self.port(),
            fs::read_to_string(self.state().join("dashboard/session")).unwrap()
        )
    }

    fn get(&self, path: &str, cookie: bool) -> Response {
        self.request(
            path,
            if cookie {
                vec![("Cookie", self.cookie())]
            } else {
                vec![]
            },
        )
    }

    fn request(&self, path: &str, headers: Vec<(&str, String)>) -> Response {
        let mut raw = Vec::new();
        connect(self.port(), path, &headers)
            .read_to_end(&mut raw)
            .unwrap();
        let end = raw
            .windows(4)
            .position(|chunk| chunk == b"\r\n\r\n")
            .unwrap();
        let head = String::from_utf8(raw[..end].to_vec()).unwrap();
        Response {
            status: head.lines().next().unwrap().to_owned(),
            headers: head
                .lines()
                .skip(1)
                .map(|line| {
                    let (name, value) = line.split_once(": ").unwrap();
                    (name.to_owned(), value.to_owned())
                })
                .collect(),
            body: raw[end + 4..].to_vec(),
        }
    }
}

impl Drop for Dashboard {
    fn drop(&mut self) {
        let pattern = format!("{} dashboard-server", self.state().display());
        let _ = Command::new("pkill").args(["-f", &pattern]).status();
        eventually(|| {
            let output = Command::new("pgrep").args(["-f", &pattern]).output().ok()?;
            (!output.status.success()).then_some(())
        });
    }
}

struct Response {
    status: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

impl Response {
    fn status(&self, code: u16) {
        assert_eq!(
            self.status.split_whitespace().nth(1).unwrap(),
            code.to_string()
        );
    }

    fn security(&self) -> BTreeMap<&str, &str> {
        [
            "Content-Security-Policy",
            "X-Frame-Options",
            "X-Content-Type-Options",
            "Referrer-Policy",
            "Cache-Control",
        ]
        .into_iter()
        .map(|name| (name, self.headers[name].as_str()))
        .collect()
    }
}

fn data(output: Output) -> Value {
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice::<Value>(&output.stdout).unwrap()["data"].clone()
}

fn connect(port: u16, path: &str, headers: &[(&str, String)]) -> TcpStream {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut request = format!("GET {path} HTTP/1.1\r\n");
    if !headers.iter().any(|(name, _)| *name == "Host") {
        request.push_str(&format!("Host: 127.0.0.1:{port}\r\n"));
    }
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).unwrap();
    stream
}

fn eventually<T>(mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if let Some(value) = probe() {
            return Some(value);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn embedded_and_directory_pages_keep_auth_and_identical_security_headers() {
    let embedded = Dashboard::new();
    let embedded_open = embedded.open(None);
    let baseline = embedded.get("/", true);
    baseline.status(200);
    assert_eq!(baseline.body, INDEX_HTML.as_bytes());
    assert_eq!(
        embedded.get("/assets/app.js", false).body,
        APP_JS.as_bytes()
    );
    assert_eq!(
        embedded.get("/assets/app.css", false).body,
        APP_CSS.as_bytes()
    );
    embedded.get("/index.html", false).status(404);
    embedded.get("/favicon.ico", false).status(204);

    let custom = Dashboard::new();
    let ui = custom.ui("dist");
    fs::write(ui.join("api"), "reserved").unwrap();
    let opened = custom.open(Some(Path::new("dist")));
    let url = opened["url"].as_str().unwrap();
    let entry = &url[url.find("/w/").unwrap()..];
    let page = entry.split('?').next().unwrap();
    let redeem = custom.get(entry, false);
    redeem.status(303);
    assert!(redeem.headers["Set-Cookie"].starts_with(&custom.cookie()));
    custom.get(entry, false).status(401);
    for path in ["/", "/index.html", "/%69ndex.html", page] {
        custom.get(path, false).status(401);
        let response = custom.get(path, true);
        response.status(200);
        assert_eq!(response.body, b"<h1>dist</h1>");
        assert_eq!(response.security(), baseline.security());
    }
    assert_eq!(opened["space_key"], embedded_open["space_key"]);
    fs::remove_file(ui.join("api")).unwrap();
    fs::create_dir(ui.join("api")).unwrap();
    fs::write(ui.join("api/spaces"), "reserved").unwrap();
    custom.get("/%61pi/spaces", false).status(404);
    custom.get("/%68ealthz", false).status(404);
    custom.get("/api/spaces", false).status(401);
    custom.get("/api/events", false).status(401);
    custom.get("/w/invalid", true).status(404);
    custom.get("/healthz", true).status(401);
    custom
        .request("/index.html", vec![("Host", "evil.example".to_owned())])
        .status(403);
    custom
        .request(
            "/index.html",
            vec![("Sec-Fetch-Site", "cross-site".to_owned())],
        )
        .status(403);
    custom.get("/assets/app.js", false).status(404);
    custom.get("/favicon.ico", false).status(404);
    custom.get("/missing", true).status(404);
    fs::remove_file(ui.join("index.html")).unwrap();
    custom.get("/", true).status(404);
}

#[test]
fn directory_assets_have_correct_types_and_are_read_fresh() {
    let dashboard = Dashboard::new();
    let ui = dashboard.ui("dist");
    fs::create_dir(ui.join("assets")).unwrap();
    let cases = [
        ("html", "text/html; charset=utf-8"),
        ("js", "text/javascript; charset=utf-8"),
        ("css", "text/css; charset=utf-8"),
        ("svg", "image/svg+xml"),
        ("png", "image/png"),
        ("json", "application/json"),
        ("woff2", "font/woff2"),
        ("map", "application/json"),
        ("bin", "application/octet-stream"),
    ];
    for (extension, _) in cases {
        fs::write(ui.join(format!("assets/hash.{extension}")), [0, 128, 255]).unwrap();
    }
    fs::write(ui.join("assets/space name.js"), "space").unwrap();
    dashboard.open(Some(&ui));
    let baseline = dashboard.get("/", true);
    for (extension, mime) in cases {
        let response = dashboard.get(&format!("/assets/hash.{extension}"), false);
        response.status(200);
        assert_eq!(response.headers["Content-Type"], mime);
        assert_eq!(response.body, [0, 128, 255]);
        assert_eq!(response.security(), baseline.security());
    }
    assert_eq!(
        dashboard.get("/assets/space%20name%2ejs", false).body,
        b"space"
    );
    fs::write(ui.join("replacement"), "fresh asset").unwrap();
    fs::rename(ui.join("replacement"), ui.join("assets/hash.js")).unwrap();
    assert_eq!(dashboard.get("/assets/hash.js", false).body, b"fresh asset");
    fs::write(ui.join("replacement"), "fresh index").unwrap();
    fs::rename(ui.join("replacement"), ui.join("index.html")).unwrap();
    assert_eq!(dashboard.get("/", true).body, b"fresh index");
}

#[test]
fn directory_requests_reject_traversal_symlinks_and_nonregular_files() {
    let dashboard = Dashboard::new();
    let ui = dashboard.ui("dist");
    fs::write(dashboard.root().join("secret"), "outside secret").unwrap();
    fs::create_dir(ui.join("assets")).unwrap();
    fs::write(ui.join("assets/real.js"), "internal").unwrap();
    symlink(dashboard.root().join("secret"), ui.join("escape")).unwrap();
    symlink(dashboard.root(), ui.join("escape-dir")).unwrap();
    symlink(ui.join("assets/real.js"), ui.join("internal")).unwrap();
    symlink(ui.join("assets"), ui.join("internal-dir")).unwrap();
    let fifo = std::ffi::CString::new(ui.join("fifo").as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    dashboard.open(Some(&ui));
    for path in [
        "/../secret",
        "/assets/../../secret",
        "/%2e%2e/secret",
        "/%2E%2E%2fsecret",
        "/assets/%2e%2e/%2e%2e/secret",
        "/assets/..%2f..%2fsecret",
        "/escape",
        "/escape-dir/secret",
        "/internal",
        "/internal-dir/real.js",
        "/fifo",
        "/assets",
        "/assets/real.js/child",
        "/%",
        "/%2",
        "/%GG",
        "/%00",
        "/%5csecret",
        "/a\\b",
    ] {
        dashboard.get(path, false).status(404);
    }
    fs::remove_file(ui.join("index.html")).unwrap();
    symlink(dashboard.root().join("secret"), ui.join("index.html")).unwrap();
    dashboard.get("/", true).status(404);
}

#[test]
fn invalid_ui_directories_name_the_rejected_path() {
    let dashboard = Dashboard::new();
    let file = dashboard.root().join("file");
    fs::write(&file, "file").unwrap();
    for path in [dashboard.root().join("missing"), file] {
        let output = dashboard.command(Some(&path)).output().unwrap();
        assert!(!output.status.success());
        let diagnostic = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(diagnostic.contains(path.to_str().unwrap()), "{diagnostic}");
    }
    assert!(!dashboard.state().join("dashboard/server.json").exists());
}

#[test]
fn server_reuse_requires_the_same_canonical_ui_setting() {
    for custom_first in [false, true] {
        let dashboard = Dashboard::new();
        let a = dashboard.ui("a");
        let b = dashboard.ui("b");
        let source = custom_first.then_some(a.as_path());
        dashboard.open(source);
        let info = dashboard.info();
        if custom_first {
            symlink(&a, dashboard.root().join("alias")).unwrap();
        }
        let same = dashboard.open(if custom_first {
            Some(Path::new("alias"))
        } else {
            None
        });
        assert_eq!(same["server"], "running");
        assert_eq!(dashboard.info(), info);
        let alternatives = if custom_first {
            vec![None, Some(b.as_path())]
        } else {
            vec![Some(a.as_path())]
        };
        for alternative in alternatives {
            let output = dashboard.command(alternative).output().unwrap();
            assert!(!output.status.success());
            let diagnostic = String::from_utf8_lossy(&output.stdout);
            assert!(
                diagnostic.contains(if custom_first {
                    a.to_str().unwrap()
                } else {
                    "embedded"
                }),
                "{diagnostic}"
            );
            assert!(
                diagnostic.contains(&format!("kill {}", info["pid"])),
                "{diagnostic}"
            );
            assert_eq!(dashboard.info(), info);
        }
    }
}

#[test]
fn concurrent_first_opens_choose_one_ui_and_reject_the_other() {
    let dashboard = Dashboard::new();
    let a = dashboard.ui("a");
    let b = dashboard.ui("b");
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = [&a, &b]
        .into_iter()
        .map(|ui| {
            let mut command = dashboard.command(Some(ui));
            let barrier = barrier.clone();
            thread::spawn(move || {
                barrier.wait();
                command.output().unwrap()
            })
        })
        .collect();
    let results: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(
        results
            .iter()
            .filter(|output| output.status.success())
            .count(),
        1,
        "{results:?}"
    );
    let winner = if results[0].status.success() { &a } else { &b };
    let loser = results
        .iter()
        .find(|output| !output.status.success())
        .unwrap();
    assert!(String::from_utf8_lossy(&loser.stdout).contains(winner.to_str().unwrap()));
    assert_eq!(
        dashboard.info()["ui"]["Directory"],
        winner.to_str().unwrap()
    );
    assert_eq!(
        dashboard.get("/", true).body,
        fs::read(winner.join("index.html")).unwrap()
    );
    assert_eq!(dashboard.open(Some(winner))["server"], "running");
}

#[test]
fn upgrade_handoff_keeps_directory_port_and_browser_session() {
    let dashboard = Dashboard::new();
    let ui = dashboard.ui("dist");
    let opened = dashboard.open(Some(&ui));
    let first = dashboard.info();
    let cookie = dashboard.cookie();
    let mut events = BufReader::new(connect(
        dashboard.port(),
        &format!("/w/{}/events", opened["space_key"].as_str().unwrap()),
        &[("Cookie", cookie.clone())],
    ));
    let mut line = String::new();
    loop {
        line.clear();
        assert_ne!(events.read_line(&mut line).unwrap(), 0);
        if line.starts_with("event: snapshot") {
            break;
        }
    }
    let runtime = dashboard.state().join("runtime");
    fs::create_dir_all(runtime.join("release")).unwrap();
    fs::copy(
        env!("CARGO_BIN_EXE_namedrop"),
        runtime.join("release/namedrop"),
    )
    .unwrap();
    symlink("release", runtime.join("current")).unwrap();
    symlink("current/namedrop", runtime.join("namedrop")).unwrap();
    let successor = eventually(|| {
        let info: Value = serde_json::from_slice(
            &fs::read(dashboard.state().join("dashboard/server.json")).ok()?,
        )
        .ok()?;
        (info["instance"] != first["instance"]).then_some(info)
    })
    .expect("upgrade publishes a successor");
    assert_eq!(successor["port"], first["port"]);
    assert_eq!(successor["ui"], first["ui"]);
    assert_eq!(dashboard.cookie(), cookie);
    assert_eq!(dashboard.get("/", true).body, b"<h1>dist</h1>");
    assert_eq!(dashboard.open(Some(&ui))["server"], "running");
}

#[test]
fn old_embedded_metadata_defaults_to_embedded_and_stale_metadata_does_not_choose_a_ui() {
    let dashboard = Dashboard::new();
    dashboard.open(None);
    let mut info = dashboard.info();
    info.as_object_mut().unwrap().remove("ui");
    fs::write(
        dashboard.state().join("dashboard/server.json"),
        serde_json::to_vec(&info).unwrap(),
    )
    .unwrap();
    assert_eq!(dashboard.open(None)["server"], "running");
    let ui = dashboard.ui("custom");
    assert!(
        !dashboard
            .command(Some(&ui))
            .output()
            .unwrap()
            .status
            .success()
    );

    let stale = Dashboard::new();
    fs::create_dir_all(stale.state().join("dashboard")).unwrap();
    info["ui"] = serde_json::json!({"Directory": ui});
    fs::write(
        stale.state().join("dashboard/server.json"),
        serde_json::to_vec(&info).unwrap(),
    )
    .unwrap();
    stale.open(None);
    assert_eq!(stale.info()["ui"], "Embedded");
    assert_eq!(stale.get("/", true).body, INDEX_HTML.as_bytes());
}

#[test]
fn the_directory_handle_stays_anchored_when_its_path_is_replaced() {
    let dashboard = Dashboard::new();
    let ui = dashboard.ui("dist");
    let outside = dashboard.ui("outside");
    dashboard.open(Some(&ui));
    fs::rename(&ui, dashboard.root().join("original")).unwrap();
    symlink(&outside, &ui).unwrap();
    assert_eq!(dashboard.get("/", true).body, b"<h1>dist</h1>");
    fs::write(
        dashboard.root().join("original/index.html"),
        "anchored and fresh",
    )
    .unwrap();
    assert_eq!(dashboard.get("/", true).body, b"anchored and fresh");
}
