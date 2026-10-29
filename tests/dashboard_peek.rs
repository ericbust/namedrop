#[path = "support/xws.rs"]
mod xws;

use namedrop::{HerdrCommandExecutor, StateNamespace, dashboard::snapshot::SpaceKey};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    net::{Shutdown, TcpStream},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Child, Command, Stdio},
    sync::{Barrier, mpsc},
    thread,
    time::{Duration, Instant},
};
use xws::Fixture;

struct Server {
    child: Child,
    port: u16,
    cookie: String,
}

#[derive(Debug, Eq, PartialEq)]
struct Response {
    status: u16,
    head: String,
    body: String,
}

fn wait_for(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for test observation"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn configure(f: &Fixture, response: Value) {
    let temporary = f.root.join("screen-next.json");
    fs::write(&temporary, serde_json::to_vec(&response).unwrap()).unwrap();
    fs::rename(temporary, f.root.join("screen.json")).unwrap();
}

fn invocations(f: &Fixture) -> Vec<Value> {
    fs::read_to_string(f.root.join("reads.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn path(socket: &str, workspace: &str, alias: &str) -> String {
    format!(
        "/w/{}/participants/{}/screen",
        SpaceKey::of(&StateNamespace::for_space(socket, workspace)).as_str(),
        xws::address_id(workspace, alias),
    )
}

impl Server {
    fn start(f: &Fixture) -> Self {
        let mut responses = serde_json::Map::new();
        for args in [
            vec!["workspace", "list"],
            vec!["worktree", "list", "--workspace", "w1", "--json"],
            vec!["pane", "list", "--workspace", "w1"],
            vec!["pane", "list", "--workspace", "w2"],
            vec!["pane", "list", "--workspace", "w3"],
        ] {
            responses.insert(
                args.join(" "),
                json!(f.herdr.execute(&args).unwrap().stdout),
            );
        }
        fs::write(
            f.root.join("responses.json"),
            serde_json::to_vec(&responses).unwrap(),
        )
        .unwrap();
        configure(f, json!({"stdout": "ready\n"}));
        let shim = f.root.join("herdr");
        fs::write(&shim, r#"#!/usr/bin/python3
import json, os, subprocess, sys, time
root = os.path.dirname(__file__)
args = sys.argv[1:]
if args[:2] == ['pane', 'read']:
    with open(os.path.join(root, 'screen.json')) as source:
        config = json.load(source)
    socket = os.environ.get('HERDR_SOCKET_PATH')
    config = config.get('targets', {}).get(socket + '|' + args[2], config)
    record = json.dumps({'args': args, 'socket': socket, 'pid': os.getpid()}) + '\n'
    descriptor = os.open(os.path.join(root, 'reads.jsonl'), os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
    os.write(descriptor, record.encode())
    os.close(descriptor)
    if config.get('require_unwrapped') and args[3:5] != ['--source', 'recent-unwrapped']:
        sys.stderr.write('screen source must preserve soft-wrapped secrets')
        sys.exit(1)
    if config.get('spawn_child'):
        child = subprocess.Popen([sys.executable, '-c', "import time,sys; f=open(sys.argv[1],'a',buffering=1)\nwhile True: f.write('alive\\n'); time.sleep(0.02)", os.path.join(root, 'heartbeat')])
        with open(os.path.join(root, 'child-pid'), 'w') as output:
            output.write(str(child.pid))
    if config.get('gate'):
        while not os.path.exists(os.path.join(root, config['gate'])):
            time.sleep(0.005)
    if config.get('invalid_utf8'):
        sys.stdout.buffer.write(b'\xff')
    else:
        sys.stdout.write(config.get('stdout', ''))
    sys.stderr.write(config.get('stderr', ''))
    sys.exit(config.get('exit', 0))
with open(os.path.join(root, 'responses.json')) as source:
    responses = json.load(source)
key = ' '.join(args)
if key in responses:
    print(responses[key])
elif args[:2] == ['worktree', 'list']:
    sys.stderr.write('{"error":{"code":"not_git_worktree"}}')
    sys.exit(1)
else:
    sys.stderr.write('unexpected Herdr invocation')
    sys.exit(1)
"#).unwrap();
        fs::set_permissions(&shim, fs::Permissions::from_mode(0o700)).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_namedrop"))
            .arg("--plugin-state")
            .arg(&f.state)
            .arg("dashboard-server")
            .env("HERDR_BIN_PATH", shim)
            .env("NAMEDROP_HERDR_TIMEOUT_MS", "10000")
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_WORKSPACE_ID")
            .env_remove("HERDR_PANE_ID")
            .env_remove("HERDR_PLUGIN_STATE_DIR")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(f.root.join("server.log")).unwrap())
            .spawn()
            .unwrap();
        let mut port = None;
        wait_for(|| {
            assert!(
                child.try_wait().unwrap().is_none(),
                "dashboard exited during startup: {}",
                fs::read_to_string(f.root.join("server.log")).unwrap()
            );
            if let Ok(bytes) = fs::read(f.state.join("dashboard/server.json"))
                && let Ok(info) = serde_json::from_slice::<Value>(&bytes)
                && info["pid"] == child.id()
            {
                port = info["port"].as_u64().map(|port| port as u16);
            }
            port.is_some()
        });
        let port = port.unwrap();
        let session = fs::read_to_string(f.state.join("dashboard/session")).unwrap();
        Self {
            child,
            port,
            cookie: format!("namedrop_dashboard_{port}={session}"),
        }
    }

    fn request(&self, path: &str, headers: &[(&str, &str)]) -> Response {
        let mut request = format!("GET {path} HTTP/1.1\r\n");
        if !headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("host"))
        {
            request.push_str(&format!("Host: 127.0.0.1:{}\r\n", self.port));
        }
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        request.push_str("\r\n");
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        read_response(stream)
    }

    fn get(&self, path: &str) -> Response {
        self.request(path, &[("Cookie", &self.cookie)])
    }
}

fn read_response(mut stream: TcpStream) -> Response {
    let mut raw = String::new();
    stream.read_to_string(&mut raw).unwrap();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap();
    Response {
        status: head.split(' ').nth(1).unwrap().parse().unwrap(),
        head: head.to_owned(),
        body: body.to_owned(),
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn reads_recent_200_lines_as_plain_text_including_terminal_participants() {
    let f = Fixture::new();
    f.db().execute("UPDATE pane_addresses SET pane_type = 'terminal' WHERE worktree_id = 'w1' AND address_id = ?1", [xws::address_id("w1", "planner")]).unwrap();
    let server = Server::start(&f);
    configure(&f, json!({"stdout": "one\ntwo\n\tthree\n"}));
    for alias in ["lead", "planner"] {
        let response = server.get(&path("socket-a", "w1", alias));
        assert_eq!(response.status, 200);
        assert_eq!(response.body, "one\ntwo\n\tthree");
        assert!(
            response
                .head
                .contains("Content-Type: text/plain; charset=utf-8\r\n")
        );
        assert!(response.head.contains("Cache-Control: no-store\r\n"));
        assert!(
            response
                .head
                .contains("X-Content-Type-Options: nosniff\r\n")
        );
    }
    let reads = invocations(&f);
    assert_eq!(reads.len(), 2);
    for (read, pane) in reads.iter().zip(["pane-1-lead", "pane-1-planner"]) {
        assert_eq!(read["socket"], "socket-a");
        assert_eq!(
            read["args"],
            json!([
                "pane",
                "read",
                pane,
                "--source",
                "recent-unwrapped",
                "--lines",
                "200"
            ])
        );
    }
}

#[test]
fn distinguishes_missing_foreign_retired_unbound_and_closed_panes() {
    let f = Fixture::new();
    f.db()
        .execute(
            "UPDATE pane_addresses SET retired = 1 WHERE address_id = ?1",
            [xws::address_id("w1", "lead")],
        )
        .unwrap();
    f.db()
        .execute(
            "UPDATE pane_bindings SET active = 0 WHERE address_id = ?1",
            [xws::address_id("w1", "planner")],
        )
        .unwrap();
    let server = Server::start(&f);
    for alias in ["lead", "planner"] {
        assert_eq!(server.get(&path("socket-a", "w1", alias)).status, 410);
    }
    let foreign = path("socket-a", "w1", "lead").replace(
        &xws::address_id("w1", "lead"),
        &xws::address_id("w2", "tester"),
    );
    assert_eq!(server.get(&foreign).status, 404);
    assert_eq!(server.get(&path("socket-a", "w1", "missing")).status, 404);
    assert_eq!(server.get(&path("socket-a", "unknown", "lead")).status, 404);
    assert!(invocations(&f).is_empty());
    configure(
        &f,
        json!({"exit": 1, "stderr": "{\"error\":{\"code\":\"pane_not_found\",\"message\":\"secret stderr\"}}"}),
    );
    let closed = server.get(&path("socket-a", "w2", "tester"));
    assert_eq!(closed.status, 410);
    assert!(!closed.body.contains("secret stderr"));
    configure(&f, json!({"stdout": "fresh pane"}));
    assert_eq!(
        server.get(&path("socket-a", "w2", "tester")).body,
        "fresh pane"
    );
    assert_eq!(invocations(&f).len(), 2);
}

#[test]
fn rejects_missing_cookie_foreign_provenance_and_malformed_paths_before_execution() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let path = path("socket-a", "w1", "lead");
    assert_eq!(server.request(&path, &[]).status, 401);
    assert_eq!(
        server
            .request(&format!("{path}?n=pretend-entry-nonce"), &[])
            .status,
        401
    );
    for headers in [
        vec![("Host", "foreign.example")],
        vec![("Host", "127.0.0.1:1")],
        vec![("Origin", "http://foreign.example")],
        vec![("Origin", "null")],
        vec![("Sec-Fetch-Site", "cross-site")],
        vec![("Sec-Fetch-Site", "same-site")],
        vec![("Sec-Fetch-Site", "unknown")],
    ] {
        let mut headers = headers;
        headers.push(("Cookie", &server.cookie));
        assert_eq!(server.request(&path, &headers).status, 403);
    }
    for malformed in [
        format!("{path}/extra"),
        format!("{path}/"),
        path.replace(&xws::address_id("w1", "lead"), ""),
        path.replace(&xws::address_id("w1", "lead"), "%zz"),
        path.replace(&xws::address_id("w1", "lead"), "a%2fb"),
    ] {
        assert_eq!(server.get(&malformed).status, 404, "{malformed}");
    }
    assert!(invocations(&f).is_empty());
    for site in ["same-origin", "none"] {
        assert_eq!(
            server
                .request(
                    &path,
                    &[
                        ("Cookie", &server.cookie),
                        ("Origin", &format!("http://127.0.0.1:{}", server.port)),
                        ("Sec-Fetch-Site", site)
                    ]
                )
                .status,
            200
        );
    }
    let encoded = path.replace(
        &xws::address_id("w1", "lead"),
        &xws::address_id("w1", "lead").replace('a', "%61"),
    );
    assert_eq!(server.get(&encoded).status, 200);
    assert_eq!(server.get(&encoded.replace("%61", "%2561")).status, 404);
    let localhost = format!("localhost:{}", server.port);
    assert_eq!(
        server
            .request(
                &path,
                &[
                    ("Cookie", &server.cookie),
                    ("Host", &localhost),
                    ("Origin", &format!("http://{localhost}"))
                ]
            )
            .status,
        200
    );
}

#[test]
fn strips_terminal_sequences_and_invisible_characters_before_secret_redaction() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let raw = concat!(
        "\u{1b}[31mred\u{1b}[0m ",
        "\u{1b}]0;hidden title\u{7}osc ",
        "\u{1b}]8;;https://hidden.example\u{1b}\\link\u{1b}]8;;\u{1b}\\ ",
        "\u{1b}Pprivate\u{1b}\\dcs ",
        "\u{1b}Xprivate\u{1b}\\sos ",
        "\u{1b}^private\u{1b}\\pm ",
        "\u{1b}_private\u{1b}\\apc ",
        "\u{9b}31mcsi\u{9b}0m ",
        "\u{9d}hidden\u{9c}osc8 ",
        "\u{90}hidden\u{9c}dcs8 ",
        "\u{98}hidden\u{9c}sos8 ",
        "\u{9e}hidden\u{9c}pm8 ",
        "\u{9f}hidden\u{9c}apc8 ",
        "\u{1b}(Bcharset ",
        "\0\r\u{8}\u{7}\u{7f}\u{85}\u{9c}clean\tline\n",
        "sk-abcdefghijklmnopqrstuvwxyz123456\n",
        "sk-abcd\u{1b}[32mefgh\u{1b}[0mijkl\u{8}mnop\u{200b}qrstuv\u{202e}wxyz123456\n",
        "\u{1b}]unterminated private title",
    );
    configure(&f, json!({"stdout": raw}));
    let response = server.get(&path("socket-a", "w1", "lead"));
    assert_eq!(response.status, 200);
    assert_eq!(
        response.body,
        "red osc link dcs sos pm apc csi osc8 dcs8 sos8 pm8 apc8 charset clean\tline\n[REDACTED TOKEN]\n[REDACTED TOKEN]"
    );
    for suffix in [
        "\u{1b}[31",
        "\u{1b}",
        "\u{1b}Pprivate",
        "\u{90}private",
        "\u{9b}31",
    ] {
        configure(&f, json!({"stdout": format!("visible{suffix}")}));
        assert_eq!(server.get(&path("socket-a", "w1", "lead")).body, "visible");
    }
}

#[test]
fn peek_redacts_pem_joiners_next_to_spaces() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let mut failures = Vec::new();
    for joiner in ['\u{200d}', '\u{2060}', '\u{200c}', '\u{fe0f}'] {
        for header in [
            format!("-----BEGIN{joiner} PRIVATE KEY-----"),
            format!("-----BEGIN {joiner}RSA PRIVATE KEY-----"),
            format!("-----BEGIN RSA PRIVATE{joiner} KEY-----"),
            format!("-----BEGIN RSA PRIVATE {joiner}KEY-----"),
        ] {
            let raw = format!(
                "👩‍💻 before\n{header}\nprivate-body\n-----END RSA PRIVATE KEY-----\nمی‌خواهم after"
            );
            configure(&f, json!({"stdout": raw}));
            let response = server.get(&path("socket-a", "w1", "lead"));
            assert_eq!(response.status, 200);
            if response.body != "👩‍💻 before\n[REDACTED PRIVATE KEY]\nمی‌خواهم after"
            {
                failures.push((raw, response.body));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "Peek PEM joiner bypasses: {failures:#?}"
    );
}

#[test]
fn peek_redacts_ansi_inside_tokens_and_pem_headers() {
    let f = Fixture::new();
    let server = Server::start(&f);
    for (raw, expected) in [
        (
            "s\u{1b}[31mk-abcdefghijklmnopqrstuvwxyz123456",
            "[REDACTED TOKEN]",
        ),
        (
            "ghp_abcdefgh\u{1b}[32mijklmnopqrstuvwxyz123456",
            "[REDACTED TOKEN]",
        ),
        ("AKIAABCD\u{9b}31mEFGHIJKLMNOP", "[REDACTED AWS ACCESS KEY]"),
        (
            "-----BEGIN\u{1b}[31m PRIVATE KEY-----\nprivate-body\n-----END PRIVATE KEY-----",
            "[REDACTED PRIVATE KEY]",
        ),
    ] {
        configure(&f, json!({"stdout": raw}));
        let response = server.get(&path("socket-a", "w1", "lead"));
        assert_eq!(response.status, 200);
        assert_eq!(response.body, expected, "{raw:?}");
    }
}

#[test]
fn peek_preserves_nonsecret_unicode_and_coloured_cli_output() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let expected = "👨‍👩‍👧‍👦 🇺🇸 🏳️‍🌈\nمی‌خواهم\n❤️ ✈︎ ✈️\nReady\t2 passed";
    let raw = "👨‍👩‍👧‍👦 🇺🇸 🏳️‍🌈\nمی‌خواهم\n❤️ ✈︎ ✈️\n\u{1b}[32mReady\u{1b}[0m\t2 passed";
    configure(&f, json!({"stdout": raw}));
    let response = server.get(&path("socket-a", "w1", "lead"));
    assert_eq!(response.status, 200);
    assert_eq!(response.body, expected);
}

#[test]
fn screen_preserves_legitimate_unicode_beside_redacted_secrets() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let raw = concat!(
        "👨‍👩‍👧 ❤️ می‌خواهم क्‍ष क्‌ष 안녕하세요 ㅤ\n",
        "foo\u{200d}bar_sk-abcdefghijklmnopqrstuvwxyz123456\n",
        "API\tKEY=hunter2-plain-secret ghp_\tabcdefghijklmnopqrstuvwxyz123456\n",
        "ghp_abcdefghijklmnopqrstuvwxyz123456=visible",
    );
    configure(&f, json!({"stdout": raw}));
    let response = server.get(&path("socket-a", "w1", "lead"));
    assert_eq!(response.status, 200);
    assert_eq!(
        response.body,
        concat!(
            "👨‍👩‍👧 ❤️ می‌خواهم क्‍ष क्‌ष 안녕하세요 ㅤ\n",
            "foo\u{200d}bar_[REDACTED TOKEN]\n",
            "API\tKEY=[REDACTED ENV VALUE]\n",
            "[REDACTED TOKEN]=visible",
        )
    );
}

#[test]
fn caps_unicode_after_redaction_and_refuses_oversized_or_invalid_output() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let path = path("socket-a", "w1", "lead");
    configure(&f, json!({"stdout": "🦀".repeat(70_000)}));
    let response = server.get(&path);
    assert_eq!(response.status, 200);
    let marker = "[Earlier screen text omitted]\n";
    assert_eq!(
        response.body,
        format!("{marker}{}", "🦀".repeat(65_536 - marker.chars().count()))
    );
    let prefix = "x".repeat(65_530);
    configure(
        &f,
        json!({"stdout": format!("{prefix} sk-abcdefghijklmnopqrstuvwxyz123456")}),
    );
    let response = server.get(&path);
    assert_eq!(response.status, 200);
    assert_eq!(response.body.chars().count(), 65_536);
    assert!(!response.body.contains("sk-"));
    for config in [
        json!({"stdout": "x".repeat(1024 * 1024 + 1)}),
        json!({"invalid_utf8": true}),
        json!({"exit": 1, "stderr": "private upstream diagnostic"}),
        json!({"exit": 1, "stderr": "pane_not_found in unstructured diagnostic"}),
    ] {
        configure(&f, config);
        let response = server.get(&path);
        assert_eq!(response.status, 503);
        assert_eq!(response.body, "Screen is temporarily unavailable.\n");
    }
    configure(&f, json!({"stdout": "recovered"}));
    assert_eq!(server.get(&path).body, "recovered");
}

fn overlapping(f: &Fixture, server: &Server, path: &str, config: Value) -> Vec<Response> {
    configure(f, config);
    let before = invocations(f).len();
    let barrier = Barrier::new(11);
    thread::scope(|scope| {
        let (sender, receiver) = mpsc::channel();
        for _ in 0..10 {
            let sender = sender.clone();
            let barrier = &barrier;
            scope.spawn(move || {
                let mut stream = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                write!(
                    stream,
                    "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: {}\r\n\r\n",
                    server.port, server.cookie
                )
                .unwrap();
                stream.shutdown(Shutdown::Write).unwrap();
                barrier.wait();
                sender.send(read_response(stream)).unwrap();
            });
        }
        barrier.wait();
        wait_for(|| invocations(f).len() > before);
        thread::sleep(Duration::from_millis(200));
        assert_eq!(invocations(f).len(), before + 1);
        assert!(
            receiver.try_recv().is_err(),
            "read completed before fake was released"
        );
        fs::write(f.root.join("release"), "").unwrap();
        let responses: Vec<_> = (0..10)
            .map(|_| receiver.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect();
        assert_eq!(invocations(f).len(), before + 1);
        responses
    })
}

#[test]
fn ten_overlapping_callers_share_success_and_later_refresh_reads_again() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let path = path("socket-a", "w1", "lead");
    let responses = overlapping(
        &f,
        &server,
        &path,
        json!({"stdout": "\u{1b}[32mshared\u{1b}[0m", "gate": "release"}),
    );
    for response in &responses {
        assert_eq!(response.status, 200);
        assert_eq!(response.body, "shared");
        assert_eq!(response, &responses[0]);
    }
    configure(&f, json!({"stdout": "fresh"}));
    assert_eq!(server.get(&path).body, "fresh");
    assert_eq!(invocations(&f).len(), 2);
}

#[test]
fn ten_overlapping_callers_share_failure_and_later_retry_reads_again() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let path = path("socket-a", "w1", "lead");
    let responses = overlapping(
        &f,
        &server,
        &path,
        json!({"exit": 1, "stderr": "private diagnostic", "gate": "release"}),
    );
    for response in &responses {
        assert_eq!(response.status, 503);
        assert_eq!(response.body, "Screen is temporarily unavailable.\n");
        assert_eq!(response, &responses[0]);
    }
    configure(&f, json!({"stdout": "retry"}));
    assert_eq!(server.get(&path).body, "retry");
    assert_eq!(invocations(&f).len(), 2);
}

fn process_stopped(pid: u64) -> bool {
    let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return true;
    };
    stat.rsplit_once(')')
        .is_some_and(|(_, state)| state.trim_start().starts_with('Z'))
}

#[test]
fn timeout_kills_and_reaps_the_reader_and_kills_its_child_before_a_fresh_retry() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let path = path("socket-a", "w1", "lead");
    configure(&f, json!({"gate": "never-release", "spawn_child": true}));
    let started = Instant::now();
    thread::scope(|scope| {
        let request = scope.spawn(|| server.get(&path));
        wait_for(|| f.root.join("child-pid").exists() && f.root.join("heartbeat").exists());
        let child: u64 = fs::read_to_string(f.root.join("child-pid"))
            .unwrap()
            .parse()
            .unwrap();
        let parent = invocations(&f)[0]["pid"].as_u64().unwrap();
        let response = request.join().unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "screen read exceeded its timeout budget"
        );
        assert_eq!(response.status, 503);
        assert_eq!(response.body, "Screen is temporarily unavailable.\n");
        assert!(
            !Path::new(&format!("/proc/{parent}")).exists(),
            "reader was not reaped"
        );
        wait_for(|| process_stopped(child));
        let size = fs::metadata(f.root.join("heartbeat")).unwrap().len();
        thread::sleep(Duration::from_millis(100));
        assert_eq!(fs::metadata(f.root.join("heartbeat")).unwrap().len(), size);
        configure(&f, json!({"stdout": "after timeout"}));
        assert_eq!(server.get(&path).body, "after timeout");
        assert_eq!(invocations(&f).len(), 2);
    });
}

#[test]
fn blocked_read_does_not_block_other_panes_or_the_same_pane_id_on_another_socket() {
    use namedrop::{
        ActorKind, MutationAttribution, PaneAddressDraft, PaneAddressId, PaneBindingId, PaneRoleId,
        StateStore,
    };
    let f = Fixture::new();
    let namespace = StateNamespace::for_space("socket-b", "w1");
    let attribution = MutationAttribution::new(ActorKind::Human, "test", 43);
    StateStore::open(&f.state)
        .unwrap()
        .transaction(|tx| {
            tx.activate_worktree(&namespace, &attribution)?;
            let draft = PaneAddressDraft {
                id: PaneAddressId::new(xws::address_id("w1", "lead")),
                role_id: PaneRoleId::new("role-b"),
                role_name: "lead".to_owned(),
                role_description: String::new(),
                alias: "lead".to_owned(),
            };
            tx.create_pane_address(&namespace, &draft, &attribution)?;
            tx.bind_pane(
                &namespace,
                &PaneBindingId::new("binding-b"),
                &draft.id,
                "pane-1-lead",
                &attribution,
            )?;
            Ok(())
        })
        .unwrap();
    let server = Server::start(&f);
    configure(
        &f,
        json!({"stdout": "independent", "targets": {
            "socket-a|pane-1-lead": {"gate": "release", "stdout": "blocked pane"}
        }}),
    );
    thread::scope(|scope| {
        let blocked = scope.spawn(|| server.get(&path("socket-a", "w1", "lead")));
        wait_for(|| invocations(&f).len() == 1);
        for target in [
            path("socket-a", "w1", "planner"),
            path("socket-b", "w1", "lead"),
        ] {
            let response = server.get(&target);
            assert_eq!(response.status, 200);
            assert_eq!(response.body, "independent");
        }
        assert_eq!(invocations(&f).len(), 3);
        fs::write(f.root.join("release"), "").unwrap();
        assert_eq!(blocked.join().unwrap().body, "blocked pane");
    });
    let reads = invocations(&f);
    assert_eq!(reads[2]["socket"], "socket-b");
    assert_eq!(reads[2]["args"][2], "pane-1-lead");
}

#[test]
fn long_key_uses_unwrapped_source_and_hides_the_entire_tail() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let key = format!("sk-proj-{}UNIQUE_SECRET_TAIL", "a".repeat(164));
    configure(
        &f,
        json!({"stdout": format!("KEY={key}\n"), "require_unwrapped": true}),
    );
    let response = server.get(&path("socket-a", "w1", "lead"));
    assert_eq!(response.status, 200, "unwrapped source must be requested");
    assert_eq!(response.body, "KEY=[REDACTED TOKEN]");
    assert_eq!(invocations(&f)[0]["args"][4], "recent-unwrapped");
}

#[test]
fn newest_unicode_lines_survive_the_cap_with_an_in_budget_marker() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let path = path("socket-a", "w1", "lead");
    let marker = "[Earlier screen text omitted]\n";
    configure(
        &f,
        json!({"stdout": format!("{}NEWEST 🦀\n", "OLD 🦀 line\n".repeat(8000))}),
    );
    let response = server.get(&path);
    assert_eq!(response.status, 200);
    assert!(response.body.starts_with(marker), "missing omission marker");
    assert!(response.body.ends_with("NEWEST 🦀"), "newest line was lost");
    assert!(response.body.chars().count() <= 65_536);
    assert!(response.body[marker.len()..].starts_with("OLD 🦀 line\n"));

    configure(
        &f,
        json!({"stdout": format!("old\n{}NEWEST", "🦀".repeat(70_000))}),
    );
    let response = server.get(&path);
    assert_eq!(response.body.chars().count(), 65_536);
    assert!(response.body.starts_with(marker));
    assert!(response.body.ends_with("NEWEST"));

    configure(
        &f,
        json!({"stdout": format!("-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\nnewest", "SECRET BODY\n".repeat(7000))}),
    );
    let response = server.get(&path);
    assert_eq!(response.body, "[REDACTED PRIVATE KEY]\nnewest");
    configure(
        &f,
        json!({"stdout": format!("{} sk-proj-{}UNIQUE_SECRET_TAIL\nnewest", "old\n".repeat(17_000), "a".repeat(164))}),
    );
    let response = server.get(&path);
    assert!(response.body.starts_with(marker));
    assert!(response.body.ends_with("[REDACTED TOKEN]\nnewest"));
    assert!(!response.body.contains("UNIQUE_SECRET_TAIL"));
    configure(
        &f,
        json!({"stdout": format!("old\nsk-proj-{}UNIQUE_SECRET_TAIL\nnewest", "a".repeat(70_000))}),
    );
    assert_eq!(server.get(&path).body, "old\n[REDACTED TOKEN]\nnewest");
}

#[test]
fn sixteen_pending_peeks_include_followers_and_leave_other_routes_available() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let target = path("socket-a", "w1", "lead");
    configure(
        &f,
        json!({"stdout": "independent", "targets": {
            "socket-a|pane-1-lead": {"gate": "release", "stdout": "shared"}
        }}),
    );
    let barrier = Barrier::new(17);
    thread::scope(|scope| {
        let (sender, receiver) = mpsc::channel();
        for _ in 0..16 {
            let barrier = &barrier;
            let sender = sender.clone();
            let server = &server;
            let target = &target;
            scope.spawn(move || {
                let mut stream = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                write!(
                    stream,
                    "GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: {}\r\n\r\n",
                    server.port, server.cookie
                )
                .unwrap();
                stream.shutdown(Shutdown::Write).unwrap();
                barrier.wait();
                sender.send(read_response(stream)).unwrap();
            });
        }
        barrier.wait();
        wait_for(|| invocations(&f).len() == 1);
        thread::sleep(Duration::from_millis(250));
        let started = Instant::now();
        let excess = server.get(&path("socket-a", "w1", "planner"));
        let elapsed = started.elapsed();
        let ordinary = server.get("/api/session");
        fs::write(f.root.join("release"), "").unwrap();
        let responses: Vec<_> = (0..16)
            .map(|_| receiver.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect();
        assert_eq!(
            excess.status, 503,
            "seventeenth peek must be refused, including followers"
        );
        assert!(
            elapsed < Duration::from_secs(1),
            "admission must not wait for a read"
        );
        assert_eq!(ordinary.status, 200);
        assert!(
            responses
                .iter()
                .all(|response| response.status == 200 && response.body == "shared")
        );
    });
    configure(&f, json!({"exit": 1}));
    for _ in 0..17 {
        assert_eq!(server.get(&target).status, 503);
    }
    configure(&f, json!({"stdout": "recovered"}));
    assert_eq!(server.get(&target).body, "recovered");
}
