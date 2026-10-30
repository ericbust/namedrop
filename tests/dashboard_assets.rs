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
    process::{Command, Output, Stdio},
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant},
};
use support::TestDirectory;

// A concurrent spawn can inherit a writable fixture fd until exec, even with CLOEXEC.
// Keep those fds in waited child processes so published executables are never busy.
fn write_executable(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> std::io::Result<()> {
    let mut child = Command::new("/bin/sh")
        .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "write-executable"])
        .arg(path.as_ref())
        .stdin(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().unwrap().write_all(contents.as_ref())?;
    let output = child.wait_with_output()?;
    assert!(output.status.success(), "{output:?}");
    Ok(())
}

fn copy_executable(source: impl AsRef<Path>, destination: impl AsRef<Path>) -> std::io::Result<()> {
    let output = Command::new("cp")
        .arg(source.as_ref())
        .arg(destination.as_ref())
        .output()?;
    assert!(output.status.success(), "{output:?}");
    Ok(())
}

struct Dashboard {
    directory: TestDirectory,
}

impl Dashboard {
    fn new() -> Self {
        let directory = TestDirectory::new();
        let herdr = directory.path().join("herdr");
        write_executable(
            &herdr,
            "#!/bin/sh\nprintf '{\"result\":{\"workspace\":{\"label\":\"test\"}}}'\n",
        )
        .unwrap();
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
        ("mjs", "text/javascript; charset=utf-8"),
        ("wasm", "application/wasm"),
        ("ico", "image/x-icon"),
        ("jpg", "image/jpeg"),
        ("jpeg", "image/jpeg"),
        ("gif", "image/gif"),
        ("webp", "image/webp"),
        ("avif", "image/avif"),
        ("woff", "font/woff"),
        ("txt", "text/plain; charset=utf-8"),
        ("bin", "application/octet-stream"),
    ];
    for (extension, _) in cases {
        for extension in [extension.to_owned(), extension.to_ascii_uppercase()] {
            fs::write(ui.join(format!("assets/hash.{extension}")), [0, 128, 255]).unwrap();
        }
    }
    fs::write(ui.join("assets/space name.js"), "space").unwrap();
    dashboard.open(Some(&ui));
    let baseline = dashboard.get("/", true);
    for (extension, mime) in cases {
        for extension in [extension.to_owned(), extension.to_ascii_uppercase()] {
            let response = dashboard.get(&format!("/assets/hash.{extension}"), false);
            response.status(200);
            assert_eq!(response.headers["Content-Type"], mime);
            assert_eq!(response.body, [0, 128, 255]);
            assert_eq!(response.security(), baseline.security());
        }
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
    let canonical_winner = winner.canonicalize().unwrap();
    let loser = results
        .iter()
        .find(|output| !output.status.success())
        .unwrap();
    assert!(String::from_utf8_lossy(&loser.stdout).contains(canonical_winner.to_str().unwrap()));
    assert_eq!(
        dashboard.info()["ui"]["Directory"],
        canonical_winner.to_str().unwrap()
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
    copy_executable(
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
fn fix_forward_recreated_and_renamed_ui_directory_serves_current_files() {
    let dashboard = Dashboard::new();
    let ui = dashboard.ui("dist");
    dashboard.open(Some(&ui));
    assert_eq!(dashboard.get("/", true).body, b"<h1>dist</h1>");
    fs::remove_dir_all(&ui).unwrap();
    fs::create_dir(&ui).unwrap();
    fs::write(ui.join("index.html"), "rebuilt directory").unwrap();
    fs::write(ui.join("asset.txt"), "rebuilt asset").unwrap();
    assert_eq!(dashboard.open(Some(&ui))["server"], "running");
    assert_eq!(dashboard.get("/", true).body, b"rebuilt directory");
    assert_eq!(dashboard.get("/asset.txt", false).body, b"rebuilt asset");
    fs::rename(&ui, dashboard.root().join("old-dist")).unwrap();
    fs::create_dir(&ui).unwrap();
    fs::write(ui.join("index.html"), "new directory").unwrap();
    assert_eq!(dashboard.get("/", true).body, b"new directory");
    fs::remove_dir_all(&ui).unwrap();
    symlink(dashboard.root().join("old-dist"), &ui).unwrap();
    dashboard.get("/", true).status(404);
}

#[test]
fn fix_forward_index_file_aliases_require_the_session_cookie() {
    let dashboard = Dashboard::new();
    let ui = dashboard.ui("dist");
    if !ui.join("INDEX.HTML").exists() {
        fs::hard_link(ui.join("index.html"), ui.join("INDEX.HTML")).unwrap();
    }
    fs::hard_link(ui.join("index.html"), ui.join("alias.html")).unwrap();
    dashboard.open(Some(&ui));
    for path in ["/INDEX.HTML", "/alias.html", "/%61lias.html"] {
        dashboard.get(path, false).status(401);
        dashboard
            .request(path, vec![("Cookie", "wrong=wrong".to_owned())])
            .status(401);
        let page = dashboard.get(path, true);
        page.status(200);
        assert_eq!(page.body, b"<h1>dist</h1>");
    }
}

fn selected_startup_fixture(dashboard: &Dashboard, startup: &str) {
    let runtime = dashboard.state().join("runtime");
    write_executable(runtime.join("release/namedrop"), format!(
        "#!/bin/sh\ncase \"$*\" in *--help*) ;; *dashboard-server*) {startup};; esac\nexec '{}' \"$@\"\n",
        env!("CARGO_BIN_EXE_namedrop")
    )).unwrap();
    symlink("release", runtime.join("current")).unwrap();
    write_executable(
        runtime.join("namedrop"),
        format!(
            "#!/bin/sh\nrm '{}'\nexec '{}' \"$@\"\n",
            runtime.join("namedrop").display(),
            env!("CARGO_BIN_EXE_namedrop")
        ),
    )
    .unwrap();
}

#[test]
fn fix_forward_handoff_refuses_a_competing_embedded_ui() {
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
    selected_startup_fixture(
        &dashboard,
        &format!(
            "touch '{}'; sleep 7",
            dashboard.root().join("successor-launching").display()
        ),
    );
    eventually(|| {
        dashboard
            .root()
            .join("successor-launching")
            .exists()
            .then_some(())
    })
    .expect("successor reaches delayed launcher");
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(6) {
        let output = dashboard.command(None).output().unwrap();
        assert!(
            !output.status.success(),
            "embedded caller took over the custom UI: {output:?}"
        );
        let diagnostic = String::from_utf8_lossy(&output.stdout);
        assert!(diagnostic.contains(ui.to_str().unwrap()), "{diagnostic}");
        assert!(diagnostic.contains("restarting"), "{diagnostic}");
        assert!(diagnostic.contains("retry"), "{diagnostic}");
        thread::sleep(Duration::from_millis(200));
    }
    let output = dashboard.command(None).output().unwrap();
    assert!(!output.status.success(), "{output:?}");
    let diagnostic = String::from_utf8_lossy(&output.stdout);
    assert!(diagnostic.contains(ui.to_str().unwrap()), "{diagnostic}");
    let successor = eventually(|| {
        let info = dashboard.info();
        (info["instance"] != first["instance"]).then_some(info)
    })
    .expect("custom successor publishes metadata");
    assert_eq!(successor["ui"], first["ui"]);
    assert_eq!(successor["port"], first["port"]);
    assert_eq!(dashboard.cookie(), cookie);
    assert_eq!(dashboard.get("/", true).body, b"<h1>dist</h1>");
}

#[cfg(target_os = "linux")]
fn resident_kib(pid: u64) -> u64 {
    fs::read_to_string(format!("/proc/{pid}/status"))
        .unwrap()
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:").map(str::trim))
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap()
}

#[cfg(target_os = "linux")]
#[test]
fn fix_forward_large_asset_memory_is_bounded() {
    let dashboard = Dashboard::new();
    let ui = dashboard.ui("dist");
    fs::File::create(ui.join("large.bin"))
        .unwrap()
        .set_len(256 * 1024 * 1024)
        .unwrap();
    dashboard.open(Some(&ui));
    let pid = dashboard.info()["pid"].as_u64().unwrap();
    let before = resident_kib(pid);
    let mut stream = connect(dashboard.port(), "/large.bin", &[]);
    let mut byte = [0];
    assert_eq!(stream.read(&mut byte).unwrap(), 1, "asset response starts");
    let deadline = Instant::now() + Duration::from_millis(900);
    let mut after = resident_kib(pid);
    while Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
        after = after.max(resident_kib(pid));
    }
    assert!(
        after.saturating_sub(before) < 32 * 1024,
        "one stalled asset retained {} KiB beyond baseline {before} KiB",
        after.saturating_sub(before)
    );
}

#[test]
fn fix_two_search_only_ancestors_allow_a_readable_ui_directory() {
    let dashboard = Dashboard::new();
    let ui = dashboard.ui("ancestor/dist");
    let ancestor = dashboard.root().join("ancestor");
    fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o311)).unwrap();
    let output = dashboard.command(Some(&ui)).output().unwrap();
    fs::set_permissions(&ancestor, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        output.status.success(),
        "searchable ancestor was refused: {output:?}"
    );
    let page = dashboard.get("/", true);
    page.status(200);
    assert_eq!(page.body, b"<h1>ancestor/dist</h1>");
}

#[test]
fn fix_two_unsearchable_ui_directory_fails_fast_with_its_path() {
    let dashboard = Dashboard::new();
    let ui = dashboard.ui("no-search");
    fs::set_permissions(&ui, fs::Permissions::from_mode(0o444)).unwrap();
    let start = Instant::now();
    let output = dashboard.command(Some(&ui)).output().unwrap();
    fs::set_permissions(&ui, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        !output.status.success(),
        "unsearchable UI was accepted: {output:?}"
    );
    let diagnostic = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(diagnostic.contains(ui.to_str().unwrap()), "{diagnostic}");
    assert!(start.elapsed() < Duration::from_secs(2));
    assert!(!dashboard.state().join("dashboard/server.json").exists());
}

fn fix_two_subscribe(dashboard: &Dashboard, opened: &Value) -> BufReader<TcpStream> {
    let mut events = BufReader::new(connect(
        dashboard.port(),
        &format!("/w/{}/events", opened["space_key"].as_str().unwrap()),
        &[("Cookie", dashboard.cookie())],
    ));
    let mut line = String::new();
    loop {
        line.clear();
        assert_ne!(events.read_line(&mut line).unwrap(), 0);
        if line.starts_with("event: snapshot") {
            return events;
        }
    }
}

#[test]
#[ignore = "requires REVIEW_OLD_BINARY pointing to the real 5552bd6 release"]
fn fix_two_handoff_to_a_release_without_claim_support_stays_running() {
    let old = std::env::var_os("REVIEW_OLD_BINARY").expect("REVIEW_OLD_BINARY");
    for custom in [false, true] {
        let dashboard = Dashboard::new();
        let ui = dashboard.ui("dist");
        let opened = dashboard.open(custom.then_some(ui.as_path()));
        let first = dashboard.info();
        let cookie = dashboard.cookie();
        let _events = fix_two_subscribe(&dashboard, &opened);
        let runtime = dashboard.state().join("runtime");
        fs::create_dir_all(runtime.join("release")).unwrap();
        copy_executable(&old, runtime.join("release/namedrop")).unwrap();
        symlink("release", runtime.join("current")).unwrap();
        symlink("current/namedrop", runtime.join("namedrop")).unwrap();
        let successor = eventually(|| {
            let info: Value = serde_json::from_slice(
                &fs::read(dashboard.state().join("dashboard/server.json")).ok()?,
            )
            .ok()?;
            (info["instance"] != first["instance"]).then_some(info)
        });
        let log =
            fs::read_to_string(dashboard.state().join("dashboard/server.log")).unwrap_or_default();
        assert!(
            successor.is_some(),
            "old release did not become the running dashboard: {log}"
        );
        let successor = successor.unwrap();
        assert_eq!(successor["port"], first["port"]);
        assert_eq!(dashboard.cookie(), cookie);
        let response = dashboard.get("/", true);
        response.status(200);
        assert_eq!(
            response.body,
            if custom {
                b"<h1>dist</h1>".as_slice()
            } else {
                INDEX_HTML.as_bytes()
            }
        );
    }
}

#[test]
fn fix_two_handoff_mismatch_says_restarting_instead_of_a_retiring_pid() {
    let dashboard = Dashboard::new();
    let ui = dashboard.ui("dist");
    let opened = dashboard.open(Some(&ui));
    let _events = fix_two_subscribe(&dashboard, &opened);
    let runtime = dashboard.state().join("runtime");
    fs::create_dir_all(runtime.join("release")).unwrap();
    selected_startup_fixture(
        &dashboard,
        &format!(
            "touch '{}'; sleep 2",
            dashboard.root().join("successor-delayed").display()
        ),
    );
    eventually(|| {
        dashboard
            .root()
            .join("successor-delayed")
            .exists()
            .then_some(())
    })
    .expect("handoff is delayed in its launcher");
    let output = dashboard.command(None).output().unwrap();
    assert!(!output.status.success(), "{output:?}");
    let diagnostic = String::from_utf8_lossy(&output.stdout);
    assert!(
        diagnostic.contains("restarting") && diagnostic.contains("retry"),
        "{diagnostic}"
    );
    assert!(
        !diagnostic.contains("kill "),
        "retiring PID advice: {diagnostic}"
    );
}

#[test]
fn fix_two_handoff_resolves_and_probes_the_release_after_acquiring_the_runtime_gate() {
    let dashboard = Dashboard::new();
    let ui = dashboard.ui("dist");
    let opened = dashboard.open(Some(&ui));
    let first = dashboard.info();
    let mut events = fix_two_subscribe(&dashboard, &opened);
    let runtime = dashboard.state().join("runtime");
    fs::create_dir_all(runtime.join("before-update")).unwrap();
    fs::create_dir_all(runtime.join("release")).unwrap();
    let gate = fs::File::create(runtime.join("commands.lock")).unwrap();
    gate.lock().unwrap();
    let premature_probe = dashboard.root().join("premature-probe");
    write_executable(
        runtime.join("before-update/namedrop"),
        format!("#!/bin/sh\ntouch '{}'\nexit 1\n", premature_probe.display()),
    )
    .unwrap();
    copy_executable(
        env!("CARGO_BIN_EXE_namedrop"),
        runtime.join("release/namedrop"),
    )
    .unwrap();
    symlink("before-update", runtime.join("current")).unwrap();
    symlink("current/namedrop", runtime.join("namedrop")).unwrap();
    let mut line = String::new();
    loop {
        line.clear();
        assert_ne!(events.read_line(&mut line).unwrap(), 0);
        if line.starts_with("event: restarting") {
            break;
        }
    }
    eventually(|| {
        fs::read_to_string(dashboard.state().join("dashboard/server.log"))
            .ok()
            .filter(|log| log.contains("dashboard: stopping: a newer release is current"))
    })
    .expect("server enters handoff while the update gate is held");
    assert!(
        !premature_probe.exists(),
        "probed a release before the update gate opened"
    );
    symlink("release", runtime.join("next-current")).unwrap();
    fs::rename(runtime.join("next-current"), runtime.join("current")).unwrap();
    drop(gate);
    let successor = eventually(|| {
        let info = dashboard.info();
        (info["instance"] != first["instance"]).then_some(info)
    })
    .expect("handoff selects the release committed behind the gate");
    assert!(!premature_probe.exists());
    assert_eq!(successor["ui"], first["ui"]);
    assert_eq!(successor["port"], first["port"]);
    assert_eq!(dashboard.get("/", true).body, b"<h1>dist</h1>");
}

#[test]
fn fix_two_probe_failure_does_not_fall_back_to_a_legacy_spawn() {
    let dashboard = Dashboard::new();
    let opened = dashboard.open(None);
    let _events = fix_two_subscribe(&dashboard, &opened);
    let runtime = dashboard.state().join("runtime");
    fs::create_dir_all(runtime.join("release")).unwrap();
    let spawned = dashboard.root().join("unexpected-legacy-spawn");
    write_executable(runtime.join("release/namedrop"), format!(
        "#!/bin/sh\ncase \"$*\" in *--help*) echo 'could not acquire --inherit-claim' >&2; exit 69;; esac\ntouch '{}'\nexit 1\n", spawned.display()
    )).unwrap();
    symlink("release", runtime.join("current")).unwrap();
    symlink("current/namedrop", runtime.join("namedrop")).unwrap();
    eventually(|| {
        fs::read_to_string(dashboard.state().join("dashboard/server.log"))
            .ok()
            .filter(|log| log.contains("capability probe failed"))
    })
    .expect("arbitrary probe errors are reported");
    assert!(
        !spawned.exists(),
        "arbitrary failure started a legacy successor"
    );
}

fn fix_three_legacy_stub(dashboard: &Dashboard, fail_start: bool) -> PathBuf {
    let release = dashboard.state().join("runtime/legacy");
    fs::create_dir_all(&release).unwrap();
    let executable = release.join("namedrop");
    let target = serde_json::to_string(env!("CARGO_BIN_EXE_namedrop")).unwrap();
    let marker = serde_json::to_string(
        dashboard
            .root()
            .join("legacy-started.json")
            .to_str()
            .unwrap(),
    )
    .unwrap();
    write_executable(&executable, format!(r#"#!/usr/bin/env python3
import json,os,sys
from pathlib import Path
for argument in sys.argv[1:]:
    if argument in ('--inherit-claim','--ui'):
        message="error: unexpected argument '"+argument+"' found\n\nUsage: namedrop dashboard-server [OPTIONS]\n\nFor more information, try '--help'.\n"
        print(json.dumps({{"protocol_version":"collab/v1","status":"rejected","operation":None,"error":{{"code":"invalid_request","message":message}}}}),flush=True)
        sys.exit(64)
if '--help' in sys.argv:
    print('Usage: namedrop dashboard-server [OPTIONS]')
    sys.exit(0)
Path({marker}).write_text(json.dumps(sys.argv[1:]))
print('legacy startup stdout',flush=True)
print('legacy startup stderr',file=sys.stderr,flush=True)
if {fail_start}:
    sys.exit(69)
state=Path(sys.argv[sys.argv.index('--plugin-state')+1])
(state/'runtime/namedrop').unlink(missing_ok=True)
os.execv({target},[{target}]+sys.argv[1:])
"#, fail_start=if fail_start {"True"} else {"False"})).unwrap();
    executable
}

#[test]
fn fix_three_legacy_stub_rolls_back_embedded_and_custom_ui() {
    for custom in [true, false] {
        let dashboard = Dashboard::new();
        let ui = dashboard.ui("dist");
        let opened = dashboard.open(custom.then_some(ui.as_path()));
        let first = dashboard.info();
        let cookie = dashboard.cookie();
        let _events = fix_two_subscribe(&dashboard, &opened);
        let executable = fix_three_legacy_stub(&dashboard, false);
        for flag in ["--inherit-claim", "--ui"] {
            let output = Command::new(&executable)
                .args(["dashboard-server", flag, "--help"])
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(64));
            let rejection: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(rejection["error"]["code"], "invalid_request");
            assert!(
                rejection["error"]["message"]
                    .as_str()
                    .unwrap()
                    .contains(&format!("unexpected argument '{flag}' found"))
            );
        }
        let runtime = dashboard.state().join("runtime");
        symlink("legacy", runtime.join("current")).unwrap();
        symlink("current/namedrop", runtime.join("namedrop")).unwrap();
        let successor = eventually(|| {
            let info = dashboard.info();
            (info["instance"] != first["instance"]).then_some(info)
        });
        let log =
            fs::read_to_string(dashboard.state().join("dashboard/server.log")).unwrap_or_default();
        assert!(successor.is_some(), "legacy stub left no dashboard: {log}");
        assert_eq!(successor.unwrap()["port"], first["port"]);
        assert_eq!(dashboard.cookie(), cookie);
        assert_eq!(dashboard.get("/", true).body, INDEX_HTML.as_bytes());
        assert!(
            log.contains("legacy startup stdout") && log.contains("legacy startup stderr"),
            "{log}"
        );
        let drops = log
            .lines()
            .filter(|line| line.contains("custom UI") && line.contains("does not support --ui"))
            .count();
        assert_eq!(drops, usize::from(custom), "{log}");
        let arguments: Vec<String> = serde_json::from_slice(
            &fs::read(dashboard.root().join("legacy-started.json")).unwrap(),
        )
        .unwrap();
        assert!(
            !arguments
                .iter()
                .any(|argument| argument == "--ui" || argument == "--inherit-claim")
        );
    }
}

#[test]
fn fix_three_inherited_successor_can_release_its_claim_for_a_legacy_rollback() {
    let dashboard = Dashboard::new();
    let ui = dashboard.ui("dist");
    let opened = dashboard.open(Some(&ui));
    let first = dashboard.info();
    let cookie = dashboard.cookie();
    let _events = fix_two_subscribe(&dashboard, &opened);
    let runtime = dashboard.state().join("runtime");
    fs::create_dir_all(runtime.join("current-release")).unwrap();
    copy_executable(
        env!("CARGO_BIN_EXE_namedrop"),
        runtime.join("current-release/namedrop"),
    )
    .unwrap();
    symlink("current-release", runtime.join("current")).unwrap();
    symlink("current/namedrop", runtime.join("namedrop")).unwrap();
    let inherited = eventually(|| {
        let info = dashboard.info();
        (info["instance"] != first["instance"]).then_some(info)
    })
    .expect("current release adopts the claim");
    assert_eq!(inherited["port"], first["port"]);
    let _successor_events = fix_two_subscribe(&dashboard, &opened);
    fix_three_legacy_stub(&dashboard, false);
    symlink("legacy", runtime.join("next-current")).unwrap();
    fs::rename(runtime.join("next-current"), runtime.join("current")).unwrap();
    let successor = eventually(|| {
        let info = dashboard.info();
        (info["instance"] != inherited["instance"]).then_some(info)
    });
    let log =
        fs::read_to_string(dashboard.state().join("dashboard/server.log")).unwrap_or_default();
    assert!(
        successor.is_some(),
        "inherited stdin retained the legacy claim: {log}"
    );
    assert_eq!(successor.unwrap()["port"], first["port"]);
    assert_eq!(dashboard.cookie(), cookie);
    assert_eq!(dashboard.get("/", true).body, INDEX_HTML.as_bytes());
    assert_eq!(
        log.lines()
            .filter(|line| line.contains("custom UI") && line.contains("does not support --ui"))
            .count(),
        1,
        "{log}"
    );
}

#[test]
fn fix_three_failed_successor_stdout_and_stderr_are_logged() {
    let dashboard = Dashboard::new();
    let opened = dashboard.open(None);
    let _events = fix_two_subscribe(&dashboard, &opened);
    fix_three_legacy_stub(&dashboard, true);
    let runtime = dashboard.state().join("runtime");
    symlink("legacy", runtime.join("current")).unwrap();
    symlink("current/namedrop", runtime.join("namedrop")).unwrap();
    let reason = eventually(|| {
        fs::read_to_string(dashboard.state().join("dashboard/server.log"))
            .ok()
            .filter(|log| {
                log.contains("legacy startup stdout") && log.contains("legacy startup stderr")
            })
    });
    let log =
        fs::read_to_string(dashboard.state().join("dashboard/server.log")).unwrap_or_default();
    assert!(
        reason.is_some(),
        "failed successor output was discarded: {log}"
    );
}

#[test]
#[ignore = "requires REVIEW_PRE_ND2_BINARY pointing to the real 158592a release"]
fn fix_three_real_pre_nd2_rollback_drops_custom_ui() {
    let old = std::env::var_os("REVIEW_PRE_ND2_BINARY").expect("REVIEW_PRE_ND2_BINARY");
    let dashboard = Dashboard::new();
    let ui = dashboard.ui("dist");
    let opened = dashboard.open(Some(&ui));
    let first = dashboard.info();
    let cookie = dashboard.cookie();
    let _events = fix_two_subscribe(&dashboard, &opened);
    let runtime = dashboard.state().join("runtime");
    fs::create_dir_all(runtime.join("pre-nd2")).unwrap();
    copy_executable(old, runtime.join("pre-nd2/namedrop")).unwrap();
    symlink("pre-nd2", runtime.join("current")).unwrap();
    symlink("current/namedrop", runtime.join("namedrop")).unwrap();
    let successor = eventually(|| {
        let info = dashboard.info();
        (info["instance"] != first["instance"]).then_some(info)
    });
    let log =
        fs::read_to_string(dashboard.state().join("dashboard/server.log")).unwrap_or_default();
    assert!(
        successor.is_some(),
        "real pre-ND2 rollback left no dashboard: {log}"
    );
    assert_eq!(successor.unwrap()["port"], first["port"]);
    assert_eq!(dashboard.cookie(), cookie);
    assert_eq!(dashboard.get("/", true).body, INDEX_HTML.as_bytes());
    assert_eq!(
        log.lines()
            .filter(|line| line.contains("custom UI") && line.contains("does not support --ui"))
            .count(),
        1,
        "{log}"
    );
}

#[test]
fn fix_three_package_switch_during_probe_starts_the_probed_executable() {
    let dashboard = Dashboard::new();
    let opened = dashboard.open(None);
    let first = dashboard.info();
    let _events = fix_two_subscribe(&dashboard, &opened);
    let runtime = dashboard.state().join("runtime");
    fs::create_dir_all(runtime.join("pinned")).unwrap();
    fix_three_legacy_stub(&dashboard, false);
    let target = serde_json::to_string(env!("CARGO_BIN_EXE_namedrop")).unwrap();
    let marker =
        serde_json::to_string(dashboard.root().join("probe-entered").to_str().unwrap()).unwrap();
    let release =
        serde_json::to_string(dashboard.root().join("release-probe").to_str().unwrap()).unwrap();
    let spawned =
        serde_json::to_string(dashboard.root().join("pinned-started").to_str().unwrap()).unwrap();
    write_executable(
        runtime.join("pinned/namedrop"),
        format!(
            r#"#!/usr/bin/env python3
import os,sys,time
from pathlib import Path
if '--help' in sys.argv:
    Path({marker}).touch()
    deadline=time.monotonic()+4
    while not Path({release}).exists() and time.monotonic()<deadline:
        time.sleep(0.01)
else:
    Path({spawned}).touch()
    state=Path(sys.argv[sys.argv.index('--plugin-state')+1])
    (state/'runtime/namedrop').unlink(missing_ok=True)
os.execv({target},[{target}]+sys.argv[1:])
"#
        ),
    )
    .unwrap();
    let packages = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(runtime.join("packages.lock"))
        .unwrap();
    symlink("pinned", runtime.join("current")).unwrap();
    symlink("current/namedrop", runtime.join("namedrop")).unwrap();
    eventually(|| {
        dashboard
            .root()
            .join("probe-entered")
            .exists()
            .then_some(())
    })
    .expect("selected executable is being probed");
    let switch = move || {
        symlink("legacy", runtime.join("next-current")).unwrap();
        fs::rename(runtime.join("next-current"), runtime.join("current")).unwrap();
    };
    let publisher = match packages.try_lock() {
        Ok(()) => {
            switch();
            drop(packages);
            None
        }
        Err(fs::TryLockError::WouldBlock) => Some(thread::spawn(move || {
            eventually(|| packages.try_lock().ok())
                .expect("package publisher resumes after handoff");
            switch();
        })),
        Err(error) => panic!("{error}"),
    };
    fs::write(dashboard.root().join("release-probe"), "go").unwrap();
    let successor = eventually(|| {
        let info = dashboard.info();
        (info["instance"] != first["instance"]).then_some(info)
    });
    if let Some(publisher) = publisher {
        publisher.join().unwrap();
    }
    let log =
        fs::read_to_string(dashboard.state().join("dashboard/server.log")).unwrap_or_default();
    assert!(
        successor.is_some(),
        "probe/spawn race left no dashboard: {log}"
    );
    assert!(
        dashboard.root().join("pinned-started").exists(),
        "spawn did not execute the probed path: {log}"
    );
    assert!(!dashboard.root().join("legacy-started.json").exists());
    assert_eq!(successor.unwrap()["port"], first["port"]);
    dashboard.get("/", true).status(200);
}

#[test]
fn fix_three_current_successor_stays_pinned_when_its_probe_changes_current() {
    let dashboard = Dashboard::new();
    let opened = dashboard.open(None);
    let first = dashboard.info();
    let _events = fix_two_subscribe(&dashboard, &opened);
    let runtime = dashboard.state().join("runtime");
    fs::create_dir_all(runtime.join("new")).unwrap();
    fix_three_legacy_stub(&dashboard, false);
    copy_executable(env!("CARGO_BIN_EXE_namedrop"), runtime.join("new/real")).unwrap();
    let started = dashboard.root().join("pinned-wrapper-started");
    write_executable(runtime.join("new/namedrop"), format!(
        "#!/bin/sh\ncase \"$*\" in *--help*) ln -s legacy '{r}/next' && mv -T '{r}/next' '{r}/current';; *) touch '{}' ;; esac\nexec '{r}/new/real' \"$@\"\n",
        started.display(), r=runtime.display()
    )).unwrap();
    symlink("new", runtime.join("current")).unwrap();
    symlink("current/namedrop", runtime.join("namedrop")).unwrap();
    let successor = eventually(|| {
        let info = dashboard.info();
        (info["instance"] != first["instance"]).then_some(info)
    });
    let log =
        fs::read_to_string(dashboard.state().join("dashboard/server.log")).unwrap_or_default();
    assert!(
        successor.is_some(),
        "wrapper changed current and prevented pinned startup: {log}"
    );
    assert!(
        started.exists(),
        "did not execute the probed wrapper: {log}"
    );
    assert!(
        !dashboard.root().join("legacy-started.json").exists(),
        "main reselected the replacement: {log}"
    );
    assert_eq!(successor.unwrap()["port"], first["port"]);
    dashboard.get("/", true).status(200);
}

#[cfg(target_os = "linux")]
#[test]
fn fix_forward_stalled_asset_writer_times_out() {
    let dashboard = Dashboard::new();
    let ui = dashboard.ui("dist");
    fs::File::create(ui.join("large.bin"))
        .unwrap()
        .set_len(128 * 1024 * 1024)
        .unwrap();
    dashboard.open(Some(&ui));
    let pid = dashboard.info()["pid"].as_u64().unwrap();
    let thread_count = || {
        fs::read_dir(format!("/proc/{pid}/task"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|task| {
                !fs::read_to_string(task.path().join("comm"))
                    .unwrap_or_default()
                    .starts_with("channel-router-")
            })
            .count()
    };
    eventually(|| (thread_count() == 2).then_some(())).expect("initial health workers finish");
    let mut stream = connect(dashboard.port(), "/large.bin", &[]);
    let mut byte = [0];
    assert_eq!(stream.read(&mut byte).unwrap(), 1);
    assert!(thread_count() > 2, "asset worker starts");
    let deadline = Instant::now() + Duration::from_secs(5);
    while thread_count() > 2 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        thread_count(),
        2,
        "stalled asset writer did not release its connection"
    );
    dashboard.get("/", true).status(200);
}

#[test]
fn directory_dot_components_are_private_and_regular_hardlinks_are_public() {
    let dashboard = Dashboard::new();
    let ui = dashboard.ui("dist");
    fs::write(ui.join(".env"), "private").unwrap();
    fs::create_dir(ui.join(".hidden")).unwrap();
    fs::write(ui.join(".hidden/asset.js"), "private").unwrap();
    fs::write(dashboard.root().join("public.txt"), "public hardlink").unwrap();
    fs::hard_link(dashboard.root().join("public.txt"), ui.join("hardlink.txt")).unwrap();
    dashboard.open(Some(&ui));
    for path in [
        "/.env",
        "/%2eenv",
        "/.hidden/asset.js",
        "/%2ehidden/asset.js",
    ] {
        dashboard.get(path, false).status(404);
    }
    let response = dashboard.get("/hardlink.txt", false);
    response.status(200);
    assert_eq!(response.body, b"public hardlink");
}

#[test]
fn directory_validation_names_unreadable_and_non_utf8_paths_without_starting() {
    let dashboard = Dashboard::new();
    let unreadable = dashboard.ui("unreadable");
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();
    let start = Instant::now();
    let output = dashboard.command(Some(&unreadable)).output().unwrap();
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(unreadable.to_str().unwrap()),
        "{output:?}"
    );
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "unreadable path waited for server startup"
    );

    #[cfg(not(target_os = "macos"))]
    {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        let invalid = dashboard
            .root()
            .join(OsString::from_vec(b"dist-\xff".to_vec()));
        fs::create_dir(&invalid).unwrap();
        let start = Instant::now();
        let output = dashboard.command(Some(&invalid)).output().unwrap();
        assert!(!output.status.success(), "{output:?}");
        let diagnostic = String::from_utf8_lossy(&output.stdout);
        assert!(
            diagnostic.contains(invalid.to_string_lossy().as_ref()),
            "{diagnostic}"
        );
        assert!(diagnostic.contains("UTF-8"), "{diagnostic}");
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "invalid UTF-8 path waited for server startup"
        );
    }
    #[cfg(target_os = "macos")]
    eprintln!(
        "skipping invalid UTF-8 path fixture on macOS because APFS rejects non-UTF-8 file names"
    );
    assert!(!dashboard.state().join("dashboard/server.json").exists());
}

#[test]
fn replaced_ui_ancestor_symlinks_are_not_followed() {
    let dashboard = Dashboard::new();
    let ui = dashboard.ui("build/dist");
    dashboard.open(Some(&ui));
    fs::rename(
        dashboard.root().join("build"),
        dashboard.root().join("old-build"),
    )
    .unwrap();
    symlink(
        dashboard.root().join("old-build"),
        dashboard.root().join("build"),
    )
    .unwrap();
    dashboard.get("/", true).status(404);
    fs::remove_file(dashboard.root().join("build")).unwrap();
    fs::create_dir_all(&ui).unwrap();
    fs::write(ui.join("index.html"), "new ancestor").unwrap();
    assert_eq!(dashboard.get("/", true).body, b"new ancestor");
}

fn inherited_server(dashboard: &Dashboard) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_namedrop"));
    command
        .args(["--plugin-state"])
        .arg(dashboard.state())
        .args(["dashboard-server", "--inherit-claim"])
        .env_remove("HERDR_PANE_ID")
        .env_remove("HERDR_WORKSPACE_ID")
        .env_remove("HERDR_SOCKET_PATH")
        .env_remove("HERDR_PLUGIN_STATE_DIR")
        .env("HERDR_BIN_PATH", dashboard.root().join("herdr"));
    command
}

#[test]
fn inherited_claim_rejects_foreign_or_separately_locked_descriptors() {
    let dashboard = Dashboard::new();
    let output = inherited_server(&dashboard)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("does not match server.lock"),
        "{output:?}"
    );
    let path = dashboard.state().join("dashboard/server.lock");
    let owner = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    owner.try_lock().unwrap();
    let other = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let output = inherited_server(&dashboard).stdin(other).output().unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("could not acquire inherited dashboard claim"),
        "{output:?}"
    );
    assert!(!dashboard.state().join("dashboard/server.json").exists());
}

#[test]
fn inherited_claim_survives_parent_close_and_is_released_after_crash() {
    let dashboard = Dashboard::new();
    fs::create_dir_all(dashboard.state().join("dashboard")).unwrap();
    let path = dashboard.state().join("dashboard/server.lock");
    let owner = fs::File::create(&path).unwrap();
    owner.try_lock().unwrap();
    let mut child = inherited_server(&dashboard)
        .stdin(owner.try_clone().unwrap())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    drop(owner);
    eventually(|| {
        dashboard
            .state()
            .join("dashboard/server.json")
            .exists()
            .then_some(())
    })
    .expect("successor adopts inherited claim");
    let contender = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    assert!(matches!(
        contender.try_lock(),
        Err(fs::TryLockError::WouldBlock)
    ));
    child.kill().unwrap();
    child.wait().unwrap();
    contender
        .try_lock()
        .expect("crash releases inherited claim");
}

#[test]
fn failed_handoff_stops_launcher_helpers_and_releases_the_claim() {
    let dashboard = Dashboard::new();
    let ui = dashboard.ui("dist");
    dashboard.open(Some(&ui));
    let opened = dashboard.open(Some(&ui));
    let mut events = BufReader::new(connect(
        dashboard.port(),
        &format!("/w/{}/events", opened["space_key"].as_str().unwrap()),
        &[("Cookie", dashboard.cookie())],
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
    let helper = dashboard.root().join("helper-pid");
    selected_startup_fixture(
        &dashboard,
        &format!(
            "exec 3<&0; sleep 60 <&3 & echo $! > '{}'; exit 1",
            helper.display()
        ),
    );
    let pid =
        eventually(|| fs::read_to_string(&helper).ok()).expect("handoff launcher starts helper");
    let contender = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dashboard.state().join("dashboard/server.lock"))
        .unwrap();
    let released = eventually(|| contender.try_lock().ok()).is_some();
    let _ = Command::new("kill")
        .arg(pid.trim())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    assert!(
        released,
        "failed handoff left its helper holding the singleton claim"
    );
    drop(contender);
    fs::remove_file(runtime.join("namedrop")).unwrap();
    assert_eq!(dashboard.open(Some(&ui))["server"], "started");
    assert_eq!(dashboard.get("/", true).body, b"<h1>dist</h1>");
}

#[test]
fn fix_four_current_handoff_keeps_server_log_lines_prefixed() {
    let dashboard = Dashboard::new();
    let opened = dashboard.open(None);
    let first = dashboard.info();
    let _events = fix_two_subscribe(&dashboard, &opened);
    let runtime = dashboard.state().join("runtime");
    fs::create_dir_all(runtime.join("release")).unwrap();
    copy_executable(
        env!("CARGO_BIN_EXE_namedrop"),
        runtime.join("release/namedrop"),
    )
    .unwrap();
    symlink("release", runtime.join("current")).unwrap();
    symlink("current/namedrop", runtime.join("namedrop")).unwrap();
    let serving = format!("dashboard: serving 127.0.0.1:{}", first["port"]);
    let log = eventually(|| {
        let info = dashboard.info();
        let log = fs::read_to_string(dashboard.state().join("dashboard/server.log")).ok()?;
        (info["instance"] != first["instance"] && log.matches(&serving).count() == 2).then_some(log)
    })
    .expect("successor publishes metadata and logs that it is serving");
    dashboard.get("/", true).status(200);
    for line in log.lines().filter(|line| !line.is_empty()) {
        assert!(
            line.starts_with("dashboard:"),
            "unexpected server.log line {line:?}"
        );
    }
}

#[test]
fn fix_four_b_same_ui_waits_for_a_ready_successor_url() {
    for custom in [false, true] {
        let dashboard = Dashboard::new();
        let ui = dashboard.ui("dist");
        let requested_ui = custom.then_some(ui.as_path());
        let opened = dashboard.open(requested_ui);
        let first = dashboard.info();
        let cookie = dashboard.cookie();
        let _events = fix_two_subscribe(&dashboard, &opened);
        let runtime = dashboard.state().join("runtime");
        fs::create_dir_all(runtime.join("release")).unwrap();
        let delayed = dashboard.root().join("successor-delayed");
        let resumed = dashboard.root().join("successor-resumed");
        selected_startup_fixture(
            &dashboard,
            &format!(
                "touch '{}'; sleep 1; touch '{}'",
                delayed.display(),
                resumed.display()
            ),
        );
        eventually(|| delayed.exists().then_some(())).expect("successor startup is delayed");
        let output = dashboard.command(requested_ui).output().unwrap();
        assert!(
            output.status.success(),
            "same-UI open failed during handoff, custom={custom}: {output:?}"
        );
        assert!(resumed.exists(), "caller returned before successor startup");
        let reopened = data(output);
        assert_eq!(reopened["server"], "running");
        assert_eq!(reopened["space_key"], opened["space_key"]);
        assert_eq!(reopened["workspace_id"], "w1");
        assert!(
            reopened["url"].as_str().unwrap().starts_with(&format!(
                "http://127.0.0.1:{}/w/{}?n=",
                first["port"],
                opened["space_key"].as_str().unwrap()
            )),
            "handoff changed the running port or requested space: {reopened}"
        );
        let url = reopened["url"].as_str().unwrap();
        let path = url
            .strip_prefix(&format!("http://127.0.0.1:{}", first["port"]))
            .unwrap();
        let entry = dashboard.get(path, false);
        entry.status(303);
        let successor = dashboard.info();
        assert_ne!(successor["instance"], first["instance"]);
        assert_ne!(successor["pid"], first["pid"]);
        assert_eq!(successor["port"], first["port"]);
        assert_eq!(successor["ui"], first["ui"]);
        assert_eq!(dashboard.cookie(), cookie);
        let page = dashboard.request(
            &format!("/w/{}", opened["space_key"].as_str().unwrap()),
            vec![("Cookie", cookie)],
        );
        page.status(200);
        assert_eq!(
            page.body,
            if custom {
                b"<h1>dist</h1>".as_slice()
            } else {
                INDEX_HTML.as_bytes()
            }
        );
    }
}

#[test]
fn fix_four_b_same_ui_times_out_without_a_url_during_a_long_handoff() {
    for custom in [false, true] {
        let dashboard = Dashboard::new();
        let ui = dashboard.ui("dist");
        let requested_ui = custom.then_some(ui.as_path());
        let opened = dashboard.open(requested_ui);
        let _events = fix_two_subscribe(&dashboard, &opened);
        let runtime = dashboard.state().join("runtime");
        fs::create_dir_all(runtime.join("release")).unwrap();
        let delayed = dashboard.root().join("successor-delayed");
        let resumed = dashboard.root().join("successor-resumed");
        selected_startup_fixture(
            &dashboard,
            &format!(
                "touch '{}'; sleep 7; touch '{}'",
                delayed.display(),
                resumed.display()
            ),
        );
        eventually(|| delayed.exists().then_some(())).expect("successor startup is delayed");
        let started = Instant::now();
        let output = dashboard.command(requested_ui).output().unwrap();
        let elapsed = started.elapsed();
        assert!(!output.status.success(), "custom={custom}: {output:?}");
        assert!(
            elapsed >= Duration::from_secs(5) && elapsed < Duration::from_millis(6500),
            "matching caller did not respect the five-second startup bound: {elapsed:?}"
        );
        assert!(
            !resumed.exists(),
            "caller waited for the seven-second successor"
        );
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response["error"]["code"], "operation_failed");
        assert!(response["data"]["url"].is_null(), "{response}");
        let diagnostic = response["error"]["message"].as_str().unwrap();
        assert!(diagnostic.contains("restarting"), "{diagnostic}");
        assert!(diagnostic.contains("retry in a moment"), "{diagnostic}");
        assert!(!diagnostic.contains("before requesting"), "{diagnostic}");
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains("http://"),
            "{response}"
        );
    }
}

#[test]
fn fix_four_b_custom_ui_waits_then_refuses_an_embedded_legacy_successor() {
    let dashboard = Dashboard::new();
    let ui = dashboard.ui("dist");
    let opened = dashboard.open(Some(&ui));
    let first = dashboard.info();
    let cookie = dashboard.cookie();
    let _events = fix_two_subscribe(&dashboard, &opened);
    let runtime = dashboard.state().join("runtime");
    let legacy = if let Some(old) = std::env::var_os("REVIEW_PRE_ND2_BINARY") {
        fs::create_dir_all(runtime.join("legacy")).unwrap();
        let legacy = runtime.join("legacy/namedrop");
        copy_executable(old, &legacy).unwrap();
        legacy
    } else {
        fix_three_legacy_stub(&dashboard, false)
    };
    fs::create_dir_all(runtime.join("release")).unwrap();
    let delayed = dashboard.root().join("successor-delayed");
    let resumed = dashboard.root().join("successor-resumed");
    write_executable(runtime.join("release/namedrop"), format!(
        "#!/bin/sh\ncase \"$*\" in *--help*) ;; *dashboard-server*) touch '{}'; sleep 2; touch '{}';; esac\nexec '{}' \"$@\"\n",
        delayed.display(), resumed.display(), legacy.display()
    )).unwrap();
    write_executable(
        runtime.join("caller-launcher"),
        format!(
            "#!/bin/sh\nrm '{}'\nexec '{}' \"$@\"\n",
            runtime.join("namedrop").display(),
            env!("CARGO_BIN_EXE_namedrop")
        ),
    )
    .unwrap();
    symlink("release", runtime.join("current")).unwrap();
    fs::rename(runtime.join("caller-launcher"), runtime.join("namedrop")).unwrap();
    eventually(|| delayed.exists().then_some(())).expect("legacy successor startup is delayed");
    let output = dashboard.command(Some(&ui)).output().unwrap();
    assert!(!output.status.success(), "{output:?}");
    assert!(
        resumed.exists(),
        "custom caller refused before successor startup"
    );
    let response: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["error"]["code"], "operation_failed");
    assert!(response["data"]["url"].is_null(), "{response}");
    assert!(
        !String::from_utf8_lossy(&output.stdout).contains("http://"),
        "{response}"
    );
    let successor = dashboard.info();
    assert_ne!(successor["instance"], first["instance"]);
    assert_ne!(successor["pid"], first["pid"]);
    assert_eq!(successor["port"], first["port"]);
    assert_eq!(dashboard.cookie(), cookie);
    let diagnostic = response["error"]["message"].as_str().unwrap();
    assert!(
        diagnostic.contains("the dashboard server is running with embedded UI"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains(&format!("stop it with `kill {}`", successor["pid"])),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains(&format!(
            "before requesting --ui {}",
            ui.canonicalize().unwrap().display()
        )),
        "{diagnostic}"
    );
    assert!(!diagnostic.contains("restarting"), "{diagnostic}");
    let page = dashboard.request(
        &format!("/w/{}", opened["space_key"].as_str().unwrap()),
        vec![("Cookie", cookie)],
    );
    page.status(200);
    assert_eq!(page.body, INDEX_HTML.as_bytes());
}

#[test]
fn fix_four_runtime_update_locks_are_available_during_slow_successor_startup() {
    let dashboard = Dashboard::new();
    let opened = dashboard.open(None);
    let _events = fix_two_subscribe(&dashboard, &opened);
    let runtime = dashboard.state().join("runtime");
    fs::create_dir_all(runtime.join("release")).unwrap();
    let delayed = dashboard.root().join("successor-delayed");
    let resumed = dashboard.root().join("successor-resumed");
    selected_startup_fixture(
        &dashboard,
        &format!(
            "touch '{}'; sleep 7; touch '{}'",
            delayed.display(),
            resumed.display()
        ),
    );
    eventually(|| delayed.exists().then_some(())).expect("successor startup is delayed");
    let acquired = ["packages.lock", "commands.lock"].map(|name| {
        let contender = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(runtime.join(name))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            assert!(
                !resumed.exists(),
                "successor resumed before testing exclusive acquisition of {name}"
            );
            match contender.try_lock() {
                Ok(()) => {
                    assert!(
                        !resumed.exists(),
                        "{name} became available only after successor resumed"
                    );
                    break true;
                }
                Err(fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(25));
                }
                Err(fs::TryLockError::WouldBlock) => break false,
                Err(fs::TryLockError::Error(error)) => panic!("{name}: {error}"),
            }
        }
    });
    let phase = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(dashboard.state().join("dashboard/handoff.lock"))
        .unwrap();
    assert!(!resumed.exists(), "successor startup is still delayed");
    assert!(
        matches!(phase.try_lock_shared(), Err(fs::TryLockError::WouldBlock)),
        "dashboard handoff lease must outlive the update locks"
    );
    assert_eq!(
        acquired,
        [true, true],
        "exclusive update locks stayed held during successor startup: packages.lock={}, commands.lock={}",
        acquired[0],
        acquired[1]
    );
}
