use super::xws::{self, Fixture};
use namedrop::dashboard::snapshot::SpaceKey;
use namedrop::{HerdrCommandExecutor, StateNamespace, StateStore};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Write},
    net::{Shutdown, TcpStream},
    os::unix::fs::PermissionsExt,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub(super) struct Server {
    child: Child,
    port: u16,
    cookie: String,
    csrf: String,
}

struct Response {
    status: u16,
    body: String,
}

impl Server {
    pub(super) fn start(f: &Fixture, response: &str, off: bool) -> Self {
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
        fs::write(
            &shim,
            r#"#!/usr/bin/python3
import json, os, sys
args = sys.argv[1:]
root = os.path.dirname(__file__)
with open(os.path.join(root, 'http-herdr-responses.json')) as source:
    responses = json.load(source)
key = ' '.join(args)
if key in responses:
    print(responses[key])
elif args[:2] == ['worktree', 'list']:
    sys.stderr.write('{"error":{"code":"not_git_worktree"}}')
    sys.exit(1)
elif args[:2] in [['agent', 'prompt'], ['pane', 'report-metadata'], ['notification', 'show']]:
    with open(os.path.join(root, 'http-herdr-invocations.jsonl'), 'a') as log:
        log.write(json.dumps({'args': args, 'socket': os.environ.get('HERDR_SOCKET_PATH')}) + '\n')
    print('{}')
else:
    sys.stderr.write('unexpected Herdr invocation: ' + key)
    sys.exit(1)
"#,
        )
        .unwrap();
        fs::set_permissions(&shim, fs::Permissions::from_mode(0o700)).unwrap();
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
            .stderr(Stdio::null());
        let router = f.root.join("fake-router");
        fs::write(f.root.join("router-response.json"), response).unwrap();
        fs::write(
            &router,
            r#"#!/usr/bin/python3
import json, os, sys, time
root = os.path.dirname(__file__)
payload = sys.stdin.read()
with open(os.path.join(root, 'router-calls.jsonl'), 'a') as log:
    log.write(json.dumps({'args': sys.argv, 'env': dict(os.environ), 'stdin': payload}) + '\n')
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
        fs::set_permissions(&router, fs::Permissions::from_mode(0o700)).unwrap();
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
        let mut child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let info = loop {
            if let Ok(bytes) = fs::read(f.state.join("dashboard/server.json"))
                && let Ok(info) = serde_json::from_slice::<Value>(&bytes)
                && info["pid"] == child.id()
            {
                break info;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("dashboard did not start");
            }
            thread::sleep(Duration::from_millis(20));
        };
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

    fn request(&self, method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> Response {
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

    fn post(&self, path: &str, body: &str, extra: &[(&str, &str)]) -> Response {
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

pub(super) fn post(
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

pub(super) fn calls(f: &Fixture) -> Vec<Value> {
    fs::read_to_string(f.root.join("router-calls.jsonl"))
        .unwrap_or_default()
        .lines()
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

pub(super) fn agent_reply(
    f: &Fixture,
    request: &str,
    parent: &Value,
    pane: &str,
    source: &str,
) -> Result<Value, String> {
    let attempt: String = f.db().query_row(
        "SELECT a.attempt_id FROM delivery_attempts a JOIN mailbox_deliveries d USING (session_socket_id,worktree_id,delivery_id,address_id)
         JOIN pane_bindings b ON b.session_socket_id=a.session_socket_id AND b.worktree_id=a.worktree_id AND b.binding_id=a.binding_id
         WHERE d.message_id=?1 AND b.pane_id=?2 ORDER BY a.rowid DESC LIMIT 1",
        [parent["message_id"].as_str().unwrap(), pane], |row| row.get(0)).unwrap_or_else(|error| panic!("missing reply attempt for {request} by {pane}: {parent}; {error}"));
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
