use super::xws::{self, Fixture};
use namedrop::dashboard::snapshot::SpaceKey;
use namedrop::{HerdrCommandExecutor, IdGenerator, ProcessIds, StateNamespace, StateStore};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    net::{Shutdown, TcpStream},
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub(super) struct Server {
    child: Child,
    port: u16,
    pub(super) cookie: String,
    csrf: String,
}

pub(super) struct Response {
    pub(super) status: u16,
    pub(super) body: String,
}

fn publish_executable(path: &Path, contents: &str) -> std::io::Result<()> {
    let staging = path.with_extension(ProcessIds::default().next_id("publish"));
    fs::write(&staging, contents)?;
    fs::set_permissions(&staging, fs::Permissions::from_mode(0o700))?;
    fs::rename(staging, path)
}

fn wait_for_start_claim(state: &Path, deadline: Instant) -> Result<(), String> {
    let path = state.join("dashboard/server.lock");
    let claim = match fs::OpenOptions::new().read(true).write(true).open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("could not open {}: {error}", path.display())),
    };
    loop {
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for predecessor to release {}",
                path.display()
            ));
        }
        match claim.try_lock() {
            Ok(()) => {
                return claim.unlock().map_err(|error| {
                    format!("could not release probe {}: {error}", path.display())
                });
            }
            Err(fs::TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(20)),
            Err(error) => return Err(format!("could not probe {}: {error}", path.display())),
        }
    }
}

fn start_server(
    command: &mut Command,
    state: &Path,
    stderr: &Path,
    timeout: Duration,
) -> Result<(Child, Value), String> {
    let started = Instant::now();
    let deadline = started + timeout;
    wait_for_start_claim(state, deadline).map_err(|error| {
        let info = fs::read_to_string(state.join("dashboard/server.json"));
        format!(
            "dashboard not spawned: {error}; elapsed={:?}; server.json={info:?}; stderr={}",
            started.elapsed(),
            stderr.display(),
        )
    })?;
    if Instant::now() >= deadline {
        return Err(format!(
            "dashboard not spawned: startup budget expired; elapsed={:?}; server.json={:?}; stderr={}",
            started.elapsed(),
            fs::read_to_string(state.join("dashboard/server.json")),
            stderr.display(),
        ));
    }
    let mut child = command.spawn().map_err(|error| {
        format!(
            "could not spawn dashboard: {error}; stderr={}",
            stderr.display()
        )
    })?;
    let info = wait_for_server(&mut child, state, stderr, started, deadline)?;
    Ok((child, info))
}

fn wait_for_server(
    child: &mut Child,
    state: &Path,
    stderr: &Path,
    started: Instant,
    deadline: Instant,
) -> Result<Value, String> {
    let (reason, status) = loop {
        let status = child.try_wait();
        match status {
            Ok(Some(_)) => break ("exited before startup", status),
            Err(_) => break ("could not inspect startup process", status),
            Ok(None) => {}
        }
        if Instant::now() >= deadline {
            break ("timed out before startup", status);
        }
        if let Ok(bytes) = fs::read(state.join("dashboard/server.json"))
            && let Ok(info) = serde_json::from_slice::<Value>(&bytes)
            && info["pid"] == child.id()
        {
            return Ok(info);
        }
        thread::sleep(Duration::from_millis(20));
    };
    let kill = if matches!(status, Ok(Some(_))) {
        None
    } else {
        Some(child.kill())
    };
    let reaped = child.wait();
    let status = match status {
        Ok(Some(status)) => status.to_string(),
        Ok(None) => "running".to_owned(),
        Err(error) => format!("unavailable ({error})"),
    };
    let output = fs::read_to_string(stderr)
        .unwrap_or_else(|error| format!("could not read stderr: {error}"));
    let info = fs::read_to_string(state.join("dashboard/server.json"))
        .unwrap_or_else(|error| format!("could not read server.json: {error}"));
    Err(format!(
        "dashboard {reason}; elapsed={:?}; pid={}; status before cleanup={status}; kill={kill:?}; reaped={reaped:?}; server.json={info}; stderr ({}):\n{output}",
        started.elapsed(),
        child.id(),
        stderr.display(),
    ))
}

impl Server {
    #[track_caller]
    pub(super) fn start(f: &Fixture, response: &str, off: bool) -> Self {
        let caller = std::panic::Location::caller();
        fs::create_dir_all(f.root.join("home/.codex")).unwrap();
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
        let responses_file = f.root.join("http-herdr-responses.json");
        fs::write(&responses_file, serde_json::to_vec(&responses).unwrap()).unwrap();
        let shim = f.root.join("http-herdr");
        publish_executable(
            &shim,
            r#"#!/usr/bin/python3
import json, os, sys, time
args = sys.argv[1:]
root = os.path.dirname(__file__)
if args == ['pane', 'list', '--workspace', 'w1']:
    gate = os.path.join(root, 'discovery-gate')
    block = os.path.exists(gate)
    if block and os.path.exists(os.path.join(root, 'discovery-single')):
        try:
            os.close(os.open(os.path.join(root, 'discovery-entered'), os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600))
        except FileExistsError:
            block = False
    if block:
        open(os.path.join(root, 'discovery-entered'), 'w').close()
        deadline = time.monotonic() + 15
        while os.path.exists(gate):
            if time.monotonic() >= deadline:
                sys.exit('discovery gate timed out')
            time.sleep(0.01)
with open(os.path.join(root, 'http-herdr-responses.json')) as source:
    responses = json.load(source)
key = ' '.join(args)
if key in responses:
    print(responses[key])
elif args[:2] == ['worktree', 'list']:
    sys.stderr.write('{"error":{"code":"not_git_worktree"}}')
    sys.exit(1)
elif args[:2] in [['agent', 'prompt'], ['agent', 'send-keys'], ['pane', 'report-metadata'], ['notification', 'show']]:
    with open(os.path.join(root, 'http-herdr-invocations.jsonl'), 'a') as log:
        log.write(json.dumps({'args': args, 'socket': os.environ.get('HERDR_SOCKET_PATH')}) + '\n')
    if args[:2] == ['agent', 'send-keys'] and os.path.exists(os.path.join(root, 'keys-gate')):
        open(os.path.join(root, 'keys-entered'), 'w').close()
        while os.path.exists(os.path.join(root, 'keys-gate')):
            time.sleep(0.01)
    if args[:2] == ['agent', 'send-keys'] and os.path.exists(os.path.join(root, 'keys-error')):
        sys.stderr.write('ambiguous interrupt transport failure')
        sys.exit(1)
    if args[:2] == ['agent', 'prompt'] and os.path.exists(os.path.join(root, 'prompt-gate')):
        open(os.path.join(root, 'prompt-entered'), 'w').close()
        while os.path.exists(os.path.join(root, 'prompt-gate')):
            time.sleep(0.01)
    print('{}')
else:
    sys.stderr.write('unexpected Herdr invocation: ' + key)
    sys.exit(1)
"#,
        )
        .unwrap();
        let stderr_path = f.root.join(format!(
            "dashboard-server-{}.stderr",
            ProcessIds::default().next_id("startup")
        ));
        let stderr = fs::File::create_new(&stderr_path).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_namedrop"));
        command
            .arg("--plugin-state")
            .arg(&f.state)
            .arg("dashboard-server")
            .env("HERDR_BIN_PATH", &shim)
            .env("HOME", f.root.join("home"))
            .env("CODEX_HOME", f.root.join("home/.codex"))
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_WORKSPACE_ID")
            .env_remove("HERDR_PANE_ID")
            .env_remove("HERDR_PLUGIN_STATE_DIR")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(stderr);
        let router = f.root.join("fake-router");
        fs::write(f.root.join("router-response.json"), response).unwrap();
        publish_executable(
            &router,
            r#"#!/usr/bin/python3
import json, os, sys, time
root = os.path.dirname(__file__)
payload = sys.stdin.read()
with open(os.path.join(root, 'router-calls.jsonl'), 'a') as log:
    log.write(json.dumps({'pid': os.getpid(), 'args': sys.argv, 'env': dict(os.environ), 'stdin': payload}) + '\n')
with open(os.path.join(root, 'router-response.json')) as source:
    output = source.read()
if output == 'timeout':
    time.sleep(30)
elif output == 'delayed':
    while not os.path.exists(os.path.join(root, 'router-release')):
        time.sleep(0.01)
    print(json.dumps({'to':['planner'], 'reason':'planning'}))
else:
    if '-o' in sys.argv:
        with open(sys.argv[sys.argv.index('-o') + 1], 'w') as result:
            result.write(output)
        print('{"type":"turn.completed","usage":{"input_tokens":100,"output_tokens":10}}')
    else:
        print(output)
"#,
        )
        .unwrap();
        command
            .env("NAMEDROP_AGENT_STATUS_CLAUDE_BIN", &router)
            .env(
                "NAMEDROP_AGENT_STATUS_CODEX_BIN",
                if f.root.join("fake-codex").exists() {
                    f.root.join("fake-codex")
                } else {
                    router.clone()
                },
            )
            .env("HERDR_CHANNEL_TEST", "secret")
            .env("NAMEDROP_CHANNEL_TEST", "secret")
            .env("CLAUDE_CODE_SESSION_ID", "secret");
        if off {
            command.env("NAMEDROP_CHANNEL_ROUTER", "off");
        } else {
            command.env_remove("NAMEDROP_CHANNEL_ROUTER");
        }
        let (child, info) = start_server(
            &mut command,
            &f.state,
            &stderr_path,
            Duration::from_secs(10),
        )
        .unwrap_or_else(|error| {
            panic!(
                "{error}; start requested at {caller}; test={:?}",
                thread::current().name()
            )
        });
        let port = info["port"].as_u64().unwrap() as u16;
        let session = fs::read_to_string(f.state.join("dashboard/session")).unwrap();
        let mut server = Self {
            child,
            port,
            cookie: format!("namedrop_dashboard_{port}={session}"),
            csrf: String::new(),
        };
        let response = server.request("GET", "/api/session", &[("Cookie", &server.cookie)], "");
        assert_eq!(response.status, 200, "{}", response.body);
        server.csrf = serde_json::from_str::<Value>(&response.body).unwrap()["csrf_token"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(server.csrf.len(), 64);
        server
    }

    fn raw(&self, request: &str) -> Response {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        let mut bytes = Vec::new();
        stream
            .read_to_end(&mut bytes)
            .expect("reading dashboard response without a reset");
        let raw = String::from_utf8(bytes).unwrap();
        let (head, body) = raw.split_once("\r\n\r\n").unwrap();
        Response {
            status: head.split(' ').nth(1).unwrap().parse().unwrap(),
            body: body.to_owned(),
        }
    }

    pub(super) fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Response {
        let mut request = format!("{method} {path} HTTP/1.1\r\n");
        if !headers
            .iter()
            .any(|(key, _)| key.eq_ignore_ascii_case("host"))
        {
            request.push_str(&format!("Host: 127.0.0.1:{}\r\n", self.port));
        }
        for (key, value) in headers {
            request.push_str(&format!("{key}: {value}\r\n"));
        }
        if method == "POST"
            && !headers
                .iter()
                .any(|(key, _)| key.eq_ignore_ascii_case("content-length"))
        {
            request.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        request.push_str("\r\n");
        request.push_str(body);
        self.raw(&request)
    }

    pub(super) fn post(&self, path: &str, body: &str, extra: &[(&str, &str)]) -> Response {
        let mut headers = vec![
            ("Cookie", self.cookie.as_str()),
            ("X-Namedrop-Csrf", self.csrf.as_str()),
            ("Content-Type", "application/json"),
        ];
        headers.extend_from_slice(extra);
        self.request("POST", path, &headers, body)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub(super) fn fixture() -> Fixture {
    let f = Fixture::new();
    f.register_all();
    f.db().execute("UPDATE integration_registrations SET runtime='claude-code' WHERE worktree_id='w1' AND address_id='address-w1-lead'", []).unwrap();
    f
}

pub(super) fn raw_post(
    server: &Server,
    request: &str,
    source: &str,
    parent: Option<&str>,
) -> (u16, Value) {
    let key = SpaceKey::of(&StateNamespace::for_space("socket-a", "w1"));
    let mut body = json!({"request_id": request, "source": source});
    if let Some(parent) = parent {
        body["in_reply_to"] = parent.into();
    }
    let response = server.post(
        &format!("/w/{}/messages", key.as_str()),
        &body.to_string(),
        &[],
    );
    (
        response.status,
        serde_json::from_str(&response.body).unwrap(),
    )
}

pub(super) fn post(
    server: &Server,
    request: &str,
    source: &str,
    parent: Option<&str>,
) -> (u16, Value) {
    let (status, mut receipt) = raw_post(server, request, source, parent);
    if status != 200 {
        return (status, receipt);
    }
    let replay = receipt["idempotent_replay"].clone();
    let deadline = Instant::now() + Duration::from_secs(20);
    while receipt["channel"]["state"] == "routing" {
        assert!(
            Instant::now() < deadline,
            "routing did not finish: {receipt}"
        );
        thread::sleep(Duration::from_millis(10));
        receipt = raw_post(server, request, source, parent).1;
    }
    receipt["idempotent_replay"] = replay;
    (status, receipt)
}

pub(super) fn calls(f: &Fixture) -> Vec<Value> {
    fs::read_to_string(f.root.join("router-calls.jsonl"))
        .unwrap_or_default()
        .split_inclusive('\n')
        .filter(|line| line.ends_with('\n'))
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

pub(super) fn history(server: &Server) -> Value {
    let key = SpaceKey::of(&StateNamespace::for_space("socket-a", "w1"));
    let response = server.request(
        "GET",
        &format!("/w/{}/messages", key.as_str()),
        &[("Cookie", &server.cookie)],
        "",
    );
    assert_eq!(response.status, 200, "{}", response.body);
    serde_json::from_str(&response.body).unwrap()
}

fn reply_attempt(f: &Fixture, parent: &Value, pane: &str, request: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match f.db().query_row(
        "SELECT a.attempt_id FROM delivery_attempts a JOIN mailbox_deliveries d USING (session_socket_id,worktree_id,delivery_id,address_id)
         JOIN delivery_attempt_state s USING (session_socket_id,worktree_id,attempt_id)
         JOIN pane_bindings b ON b.session_socket_id=a.session_socket_id AND b.worktree_id=a.worktree_id AND b.binding_id=a.binding_id
         WHERE d.message_id=?1 AND b.pane_id=?2 AND s.state <> 'dispatching' ORDER BY a.rowid DESC LIMIT 1",
        [parent["message_id"].as_str().unwrap(), pane], |row| row.get::<_, String>(0)) {
            Ok(attempt) => return attempt,
            Err(rusqlite::Error::QueryReturnedNoRows) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("missing reply attempt for {request} by {pane}: {parent}; {error}"),
        }
    }
}

pub(super) fn agent_reply(
    f: &Fixture,
    request: &str,
    parent: &Value,
    pane: &str,
    source: &str,
) -> Result<Value, String> {
    let attempt = reply_attempt(f, parent, pane, request);
    namedrop::execute_message_operation(
        "reply",
        json!({
            "request_id":request, "source":source, "in_reply_to": parent["message_id"], "author_kind":"agent_session", "attempt_id":attempt
        }),
        &f.runtime("w1", pane),
    )
}

pub(super) fn plain_root(f: &Fixture, request: &str, source: &str) -> Value {
    f.db().execute("UPDATE integration_registrations SET runtime='claude-code' WHERE worktree_id='w1' AND address_id='address-w1-lead'", []).unwrap();
    let server = Server::start(f, r#"{"to":["planner"],"reason":"planning"}"#, false);
    let (status, receipt) = post(
        &server,
        request,
        &format!("{source} Please ask @planner."),
        None,
    );
    assert_eq!(status, 200, "{receipt}");
    reply_attempt(f, &receipt, "pane-1-planner", request);
    receipt
}

pub(super) fn events(server: &Server) -> TcpStream {
    let key = SpaceKey::of(&StateNamespace::for_space("socket-a", "w1"));
    let mut stream = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "GET /w/{}/events HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: {}\r\n\r\n",
        key.as_str(),
        server.port,
        server.cookie
    )
    .unwrap();
    stream
}

pub(super) fn next_frame(stream: &mut TcpStream, buffered: &mut String) -> Value {
    loop {
        while let Some(end) = buffered.find("\n\n") {
            let event: String = buffered.drain(..end + 2).collect();
            if let Some(data) = event.lines().find_map(|line| line.strip_prefix("data: ")) {
                return serde_json::from_str(data).unwrap();
            }
        }
        let mut bytes = [0; 65536];
        let length = stream.read(&mut bytes).unwrap();
        assert_ne!(length, 0, "event stream closed");
        buffered.push_str(&String::from_utf8_lossy(&bytes[..length]));
    }
}

pub(super) fn recipient_aliases(receipt: &Value) -> Vec<&str> {
    receipt["recipients"]
        .as_array()
        .unwrap()
        .iter()
        .map(|recipient| recipient["alias"].as_str().unwrap())
        .collect()
}

pub(super) fn cancel_and_remove(f: &Fixture, workspace: &str, alias: &str) {
    StateStore::open(&f.state)
        .unwrap()
        .transaction(|transaction| {
            transaction.cancel_and_remove_pane_address(
                &StateNamespace::for_space("socket-a", workspace),
                &namedrop::PaneAddressId::new(xws::address_id(workspace, alias)),
                &namedrop::MutationAttribution::new(namedrop::ActorKind::Human, "test", 44),
            )
        })
        .unwrap();
}

pub(super) fn install_fake_codex(f: &Fixture) {
    let fake = f.root.join("fake-codex.c");
    fs::write(
        &fake,
        r#"
#include <stdio.h>
#include <string.h>
int main(int argc, char **argv) {
    char input[65536];
    fread(input, 1, sizeof(input), stdin);
    for (int i = 1; i < argc; i++) {
        if (!strcmp(argv[i-1], "-o")) {
            FILE *result = fopen(argv[i], "w");
            if (!result) return 2;
            fputs("{\"to\":[\"planner\"],\"reason\":\"planning\"}", result);
            fclose(result);
        }
    }
    puts("{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":100,\"output_tokens\":10}}");
    return 0;
}
"#,
    )
    .unwrap();
    let mut compiler = Command::new("cc");
    #[cfg(target_os = "linux")]
    compiler.arg("-static");
    let output = compiler
        .arg("-O2")
        .arg("-o")
        .arg(f.root.join("fake-codex"))
        .arg(fake)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

pub(super) fn context(server: &Server, message: &str) -> Value {
    let key = SpaceKey::of(&StateNamespace::for_space("socket-a", "w1"));
    let response = server.request(
        "GET",
        &format!("/w/{}/messages/{message}/context", key.as_str()),
        &[("Cookie", &server.cookie)],
        "",
    );
    assert_eq!(response.status, 200, "{}", response.body);
    serde_json::from_str(&response.body).unwrap()
}

#[test]
fn startup_reports_claim_deadline_without_spawning_a_child() {
    let f = Fixture::new();
    fs::create_dir_all(f.state.join("dashboard")).unwrap();
    let claim = fs::File::create_new(f.state.join("dashboard/server.lock")).unwrap();
    claim.lock().unwrap();
    let spawned = f.root.join("unexpected-child");
    let stderr = f.root.join("claim-timeout.stderr");
    let mut command = Command::new("/bin/sh");
    command
        .args(["-c", "touch \"$1\"", "probe"])
        .arg(&spawned)
        .stderr(fs::File::create_new(&stderr).unwrap());
    let error = start_server(&mut command, &f.state, &stderr, Duration::from_millis(200))
        .expect_err("predecessor keeps its claim past the deadline");
    assert!(
        error.contains("dashboard not spawned: timed out waiting for predecessor"),
        "{error}"
    );
    assert!(!spawned.exists(), "child must not run while claim is held");
    assert!(!error.contains("status before cleanup="), "{error}");
}

#[test]
fn startup_waits_for_predecessor_claim_release_before_launching_a_successor() {
    struct ClaimHolder {
        child: Child,
        release: std::path::PathBuf,
    }
    impl Drop for ClaimHolder {
        fn drop(&mut self) {
            let _ = fs::write(&self.release, "");
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    let f = fixture();
    fs::create_dir_all(f.state.join("dashboard")).unwrap();
    let entered = f.root.join("claim-entered");
    let release = f.root.join("claim-release");
    let mut holder = ClaimHolder {
        child: Command::new("/usr/bin/python3")
            .args([
                "-c",
                "import fcntl, os, sys, time\nwith open(sys.argv[1], 'a') as claim:\n fcntl.flock(claim, fcntl.LOCK_EX)\n open(sys.argv[2], 'w').close()\n while not os.path.exists(sys.argv[3]): time.sleep(0.01)",
            ])
            .arg(f.state.join("dashboard/server.lock"))
            .arg(&entered)
            .arg(&release)
            .spawn()
            .unwrap(),
        release,
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    while !entered.exists() {
        assert!(holder.child.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline, "claim holder did not start");
        thread::sleep(Duration::from_millis(10));
    }
    thread::scope(|scope| {
        let (sender, receiver) = std::sync::mpsc::channel();
        let fixture = &f;
        scope.spawn(move || {
            let started = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                Server::start(fixture, "{}", false)
            }))
            .map_err(|error| {
                error
                    .downcast_ref::<String>()
                    .cloned()
                    .unwrap_or_else(|| "startup panicked".to_owned())
            });
            sender.send(started).unwrap();
        });
        let before_release = receiver.recv_timeout(Duration::from_millis(200));
        fs::write(&holder.release, "").unwrap();
        assert!(holder.child.wait().unwrap().success());
        match before_release {
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Ok(Err(error)) => {
                panic!("startup returned before predecessor released its claim: {error}")
            }
            Ok(Ok(_)) => panic!("successor started while predecessor still held its claim"),
            Err(error) => panic!("startup worker failed: {error}"),
        }
        let server = receiver
            .recv_timeout(Duration::from_secs(10))
            .expect("startup completes after predecessor releases its claim")
            .unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(
            server
                .request("GET", "/api/session", &[("Cookie", &server.cookie)], "")
                .status,
            200
        );
    });
}

#[test]
fn startup_diagnostics_report_early_exit_and_stderr_without_waiting_for_timeout() {
    let f = Fixture::new();
    let stderr = f.root.join("early-exit.stderr");
    let mut child = Command::new("/usr/bin/python3")
        .args([
            "-c",
            "import os; os.write(2, b'x' * 262144); os.write(2, b'\\nstartup failure marker\\n'); raise SystemExit(69)",
        ])
        .stderr(fs::File::create_new(&stderr).unwrap())
        .spawn()
        .unwrap();
    let started = Instant::now();
    let error = wait_for_server(
        &mut child,
        &f.state,
        &stderr,
        started,
        started + Duration::from_secs(10),
    )
    .expect_err("child exited before publishing server.json");
    assert!(error.contains("exited before startup"), "{error}");
    assert!(
        error.contains("status before cleanup=exit status: 69"),
        "{error}"
    );
    assert!(error.contains("startup failure marker"), "{error}");
    assert!(error.contains(&"x".repeat(262144)));
    assert_eq!(child.wait().unwrap().code(), Some(69));
}

#[test]
fn startup_diagnostics_distinguish_timeout_from_the_cleanup_signal_and_reap_child() {
    let f = Fixture::new();
    let stderr = f.root.join("timeout.stderr");
    let mut child = Command::new("/bin/sh")
        .args([
            "-c",
            "printf 'startup blocked marker\\n' >&2; exec /bin/sleep 30",
        ])
        .stderr(fs::File::create_new(&stderr).unwrap())
        .spawn()
        .unwrap();
    let ready = Instant::now() + Duration::from_secs(10);
    while !fs::read_to_string(&stderr)
        .unwrap()
        .contains("startup blocked marker")
    {
        if Instant::now() >= ready || child.try_wait().unwrap().is_some() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("timeout fixture did not emit its ready marker");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let started = Instant::now();
    let error = wait_for_server(
        &mut child,
        &f.state,
        &stderr,
        started,
        started + Duration::from_millis(200),
    )
    .expect_err("live child never published server.json");
    assert!(error.contains("timed out before startup"), "{error}");
    assert!(error.contains("status before cleanup=running"), "{error}");
    assert!(error.contains("startup blocked marker"), "{error}");
    assert!(error.contains("reaped=Ok("), "{error}");
    assert!(!child.wait().unwrap().success());
}

#[test]
fn fixture_executable_can_be_replaced_while_its_previous_version_runs() {
    let f = Fixture::new();
    let executable = f.root.join("running-shim");
    fs::copy("/bin/sleep", &executable).unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let mut previous = Command::new(&executable).arg("30").spawn().unwrap();
    let published = publish_executable(&executable, "#!/bin/sh\nprintf 'replacement-version\\n'\n");
    let replacement = published
        .as_ref()
        .ok()
        .map(|_| Command::new(&executable).output());
    let previous_running = previous.try_wait().unwrap().is_none();
    let _ = previous.kill();
    previous.wait().unwrap();

    published.expect("publishing a replacement while the previous executable runs");
    assert!(
        previous_running,
        "previous executable must still be running"
    );
    let replacement = replacement.unwrap().unwrap();
    assert!(replacement.status.success());
    assert_eq!(replacement.stdout, b"replacement-version\n");
}

#[test]
fn fake_router_observation_waits_for_a_complete_jsonl_record() {
    let f = fixture();
    let path = f.root.join("router-calls.jsonl");
    let first = json!({"request_id":"complete"});
    let body = "x".repeat(8192);
    fs::write(&path, format!("{first}\n{{\"body\":\"{body}")).unwrap();
    assert_eq!(calls(&f), vec![first.clone()]);
    fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"\"}\n")
        .unwrap();
    assert_eq!(calls(&f), vec![first, json!({"body":body})]);
}
