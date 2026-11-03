#[path = "support/cleanup.rs"]
mod cleanup;

#[path = "support/xws.rs"]
mod xws;

use namedrop::{HerdrCommandExecutor, StateNamespace, dashboard::snapshot::SpaceKey};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    os::unix::fs::PermissionsExt,
    process::{Child, Command, Stdio},
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
    let temporary = f.root.join("observer-next.json");
    fs::write(&temporary, serde_json::to_vec(&response).unwrap()).unwrap();
    fs::rename(temporary, f.root.join("observer.json")).unwrap();
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
        "/w/{}/participants/{}/live",
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
        configure(f, json!({}));
        let shim = f.root.join("herdr");
        fs::write(&shim, r#"#!/usr/bin/python3
import base64,json,os,subprocess,sys,time
root=os.path.dirname(__file__);args=sys.argv[1:]
if args[:3]==['terminal','session','observe']:
    config=json.load(open(os.path.join(root,'observer.json')))
    child=None
    if config.get('child'):
        child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(120)'])
    with open(os.path.join(root,'reads.jsonl'),'a') as log:
        log.write(json.dumps({'args':args,'socket':os.environ.get('HERDR_SOCKET_PATH'),'pid':os.getpid(),'child':child.pid if child else None,'stdin_null':os.read(0,1)==b'','workspace':os.environ.get('HERDR_WORKSPACE_ID'),'pane':os.environ.get('HERDR_PANE_ID')})+'\n')
    if config.get('exit') is not None:
        os.write(1,config.get('stdout','').encode())
        sys.exit(config['exit'])
    if config.get('gate'):
        while not os.path.exists(os.path.join(root,config['gate'])):time.sleep(.01)
    if config.get('mutate_metadata'):
        json.dump(config['mutate_metadata'],open(os.path.join(root,'responses.json'),'w'))
    if config.get('raw') is not None:
        os.write(1,config['raw'].encode());sys.exit(0)
    if config.get('oversize'):
        os.write(1,b'x'*(2*1024*1024+1));time.sleep(120)
    frames=config.get('frames',[{'type':'terminal.frame','encoding':'ansi','full':True,'width':120,'height':40,'seq':1,'bytes':base64.b64encode(b'\x1b[2J\x1b[HSAFE-INITIAL').decode()}])
    for frame in frames: print(json.dumps(frame),flush=True)
    if config.get('flood'):
        seq=len(frames)
        payload=base64.b64encode(b'\x1b[H'+b'X'*60000).decode()
        while True:
            seq+=1;print(json.dumps({'type':'terminal.frame','encoding':'ansi','full':False,'width':120,'height':40,'seq':seq,'bytes':payload}),flush=True)
    if config.get('quiet'):
        while True:time.sleep(.05)
    if config.get('closed',True): print(json.dumps({'type':'terminal.closed','reason':'PRIVATE-NATIVE-REASON'}),flush=True)
    sys.exit(0)
if args[:2]==['pane','read']:print('SANITIZED SNAPSHOT');sys.exit(0)
responses=json.load(open(os.path.join(root,'responses.json')))
key=' '.join(args)
if key in responses:print(responses[key])
elif args[:2]==['worktree','list']:sys.stderr.write('{"error":{"code":"not_git_worktree"}}');sys.exit(1)
else:sys.exit(1)
"#).unwrap();
        fs::set_permissions(&shim, fs::Permissions::from_mode(0o700)).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_namedrop"))
            .arg("--plugin-state")
            .arg(&f.state)
            .arg("dashboard-server")
            .env("TMPDIR", &f.root)
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

struct Live {
    status: u16,
    reader: BufReader<TcpStream>,
}

impl Server {
    fn live(&self, target: &str) -> Live {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(
            stream,
            "GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: {}\r\n\r\n",
            self.port, self.cookie
        )
        .unwrap();
        let mut reader = BufReader::new(stream);
        let mut head = String::new();
        loop {
            let mut line = String::new();
            assert!(reader.read_line(&mut line).unwrap() > 0);
            if line == "\r\n" {
                break;
            }
            head.push_str(&line);
        }
        Live {
            status: head.split(' ').nth(1).unwrap().parse().unwrap(),
            reader,
        }
    }
}
impl Live {
    fn record(&mut self) -> Option<Value> {
        loop {
            let mut line = String::new();
            if self.reader.read_line(&mut line).unwrap() == 0 {
                return None;
            }
            if !line.trim().is_empty() {
                return Some(serde_json::from_str(&line).unwrap());
            }
        }
    }
}

fn stopped(pid: u64) -> bool {
    #[cfg(target_os = "linux")]
    {
        let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return true;
        };
        stat.rsplit_once(')')
            .is_some_and(|(_, state)| state.trim_start().starts_with('Z'))
    }
    #[cfg(not(target_os = "linux"))]
    {
        // SAFETY: signal 0 only tests existence of the fixture PID.
        (unsafe { libc::kill(i32::try_from(pid).unwrap(), 0) }) < 0
    }
}

#[test]
fn live_is_authenticated_targeted_ndjson_and_preserves_snapshot() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let response = server.get(&path("socket-a", "w1", "lead"));
    assert_eq!(response.status, 200);
    assert!(
        response
            .head
            .contains("Content-Type: application/x-ndjson\r\n")
    );
    assert!(response.head.contains("Cache-Control: no-store\r\n"));
    assert!(response.head.contains("Content-Security-Policy:"));
    assert!(response.head.contains("Content-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'none'\r\n"));
    let records: Vec<Value> = response
        .body
        .lines()
        .filter(|s| !s.is_empty())
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0]["kind"], "reset");
    assert_eq!(records[0]["seq"], 1);
    assert_eq!(records[0]["cols"], 120);
    assert_eq!(records[0]["rows"], 40);
    assert_eq!(records[0]["v"], 1);
    assert_eq!(records[0]["continuity"].as_str().unwrap().len(), 64);
    assert_eq!(
        records[1],
        json!({"v":1,"seq":2,"kind":"closed","reason":"ended"})
    );
    assert!(!response.body.contains("PRIVATE-NATIVE-REASON"));
    let calls = invocations(&f);
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0]["args"],
        json!(["terminal", "session", "observe", "t-pane-1-lead"])
    );
    assert_eq!(calls[0]["socket"], "socket-a");
    assert_eq!(calls[0]["stdin_null"], true);
    assert!(calls[0]["pane"].is_null() && calls[0]["workspace"].is_null());
    assert_eq!(
        server
            .get(&path("socket-a", "w1", "lead").replace("/live", "/screen"))
            .body,
        "SANITIZED SNAPSHOT"
    );
}

#[test]
fn auth_namespace_query_body_and_method_fail_before_observer_spawn() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let p = path("socket-a", "w1", "lead");
    assert_eq!(server.request(&p, &[]).status, 401);
    for headers in [
        vec![("Host", "other.example")],
        vec![("Origin", "http://evil.example")],
        vec![("Sec-Fetch-Site", "cross-site")],
        vec![("Sec-Fetch-Site", "same-site")],
    ] {
        let mut headers = headers;
        headers.push(("Cookie", &server.cookie));
        assert_eq!(server.request(&p, &headers).status, 403);
    }
    for query in [
        "?target=bad",
        "?continuity=",
        "?continuity=abc",
        "?continuity=bad&continuity=bad",
        "?cols=80",
        "?",
    ] {
        assert_eq!(server.get(&(p.clone() + query)).status, 400);
    }
    assert_eq!(
        server
            .request(&p, &[("Cookie", &server.cookie), ("Content-Length", "1")])
            .status,
        400
    );
    assert_eq!(
        server
            .get(&p.replace("address-w1-lead", "address-w2-lead"))
            .status,
        404
    );
    assert_eq!(
        server.get(&p.replace("address-w1-lead", "a%2Fb")).status,
        404
    );
    let mut stream = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    write!(
        stream,
        "PUT {p} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: {}\r\n\r\n",
        server.port, server.cookie
    )
    .unwrap();
    assert_eq!(read_response(stream).status, 405);
    let mut stream = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    write!(
        stream,
        "POST {p} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: {}\r\n\r\n",
        server.port, server.cookie
    )
    .unwrap();
    assert_eq!(read_response(stream).status, 405);
    assert!(invocations(&f).is_empty());
    assert_eq!(server.get(&p).status, 200);
    assert_eq!(invocations(&f).len(), 1);
}

#[test]
fn native_full_sequence_dimensions_and_split_delta_are_normalized() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let p = path("socket-a", "w1", "lead");
    let frame = |seq, full, width, bytes| json!({"type":"terminal.frame","encoding":"ansi","seq":seq,"full":full,"width":width,"height":10,"bytes":bytes});
    configure(
        &f,
        json!({"frames":[frame(1,true,40,"G1tISGk="),frame(2,false,40,"G1szMW0="),frame(3,true,50,"G1tITmV3")]}),
    );
    let response = server.get(&p);
    assert_eq!(response.status, 200);
    let values: Vec<Value> = response
        .body
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert_eq!(
        values
            .iter()
            .map(|v| v["kind"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["reset", "data", "reset", "closed"]
    );
    assert_eq!(values[0]["continuity"], values[2]["continuity"]);
    assert_eq!(values[2]["cols"], 50);
    for (i, value) in values.iter().enumerate() {
        assert_eq!(value["seq"], i + 1);
    }
    for frames in [
        vec![frame(2, true, 40, "eA==")],
        vec![frame(1, false, 40, "eA==")],
        vec![frame(1, true, 0, "eA==")],
        vec![frame(1, true, 40, "eA=="), frame(3, false, 40, "eQ==")],
        vec![frame(1, true, 40, "eA=="), frame(2, false, 41, "eQ==")],
        vec![frame(1, true, 40, "eB==")],
    ] {
        configure(&f, json!({"frames":frames}));
        let response = server.get(&p);
        assert_eq!(response.status, 200);
        assert!(
            response.body.contains("invalid_source"),
            "{}",
            response.body
        );
    }
    configure(&f, json!({"raw":"{broken\n"}));
    assert!(server.get(&p).body.contains("invalid_source"));
    configure(&f, json!({"oversize":true}));
    assert!(server.get(&p).body.contains("invalid_source"));
}

#[test]
fn unsupported_requires_empty_stdout_even_when_partial_output_exits_two() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let p = path("socket-a", "w1", "lead");
    for exit in [0, 1, 2] {
        configure(&f, json!({"stdout":"{\"type\":", "exit":exit}));
        let response = server.get(&p);
        assert_eq!(
            response.status, 503,
            "partial stdout + exit {exit}: {response:?}"
        );
        assert!(!response.body.contains("unsupported"));
    }
    configure(&f, json!({"stdout":" ", "exit":2}));
    assert_eq!(
        server.get(&p).status,
        503,
        "any stdout prevents capability fallback"
    );

    configure(&f, json!({"exit":2}));
    let empty = server.get(&p);
    assert_eq!(empty.status, 200);
    assert_eq!(
        serde_json::from_str::<Value>(empty.body.trim()).unwrap(),
        json!({"v":1,"seq":1,"kind":"unavailable","reason":"unsupported"})
    );

    configure(&f, json!({"stdout":"{broken\n", "exit":2}));
    let malformed = server.get(&p);
    assert_eq!(malformed.status, 200);
    assert!(malformed.body.contains("invalid_source"));

    let first = json!({"type":"terminal.frame","encoding":"ansi","full":true,"width":120,"height":40,"seq":1,"bytes":"eA=="});
    configure(
        &f,
        json!({"stdout":format!("{first}\n{{\"type\":"), "exit":2}),
    );
    let established = server.get(&p);
    assert_eq!(established.status, 200);
    assert!(established.body.contains("source_lost"));
    assert!(!established.body.contains("unsupported"));
}

#[test]
fn eof_is_interruption_unsupported_is_specific_and_startup_has_deadline() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let p = path("socket-a", "w1", "lead");
    configure(&f, json!({"closed":false}));
    let response = server.get(&p);
    assert!(response.body.contains("source_lost"));
    assert!(!response.body.contains("ended"));
    let first = json!({"type":"terminal.frame","encoding":"ansi","full":true,"width":120,"height":40,"seq":1,"bytes":"eA=="});
    configure(&f, json!({"raw":format!("{first}\n{{\"type\":")}));
    let partial = server.get(&p);
    assert!(partial.body.contains("source_lost"));
    assert!(!partial.body.contains("invalid_source"));
    configure(&f, json!({"exit":2}));
    assert!(server.get(&p).body.contains("unsupported"));
    configure(&f, json!({"exit":1}));
    assert_eq!(server.get(&p).status, 503);
    configure(&f, json!({"gate":"never","child":true}));
    let started = Instant::now();
    assert_eq!(server.get(&p).status, 503);
    assert!(started.elapsed() < Duration::from_secs(7));
    for call in invocations(&f) {
        wait_for(|| stopped(call["pid"].as_u64().unwrap()));
        if let Some(pid) = call["child"].as_u64() {
            wait_for(|| stopped(pid));
        }
    }
}

#[test]
fn continuity_refuses_host_and_recorded_session_changes_and_server_restart() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let p = path("socket-a", "w1", "lead");
    let first = server.get(&p);
    let reset: Value = serde_json::from_str(first.body.lines().next().unwrap()).unwrap();
    let retry = format!("{p}?continuity={}", reset["continuity"].as_str().unwrap());
    assert_eq!(server.get(&retry).status, 200);
    let before = invocations(&f).len();
    let rp = f.root.join("responses.json");
    let original = fs::read_to_string(&rp).unwrap();
    let mut responses: Value = serde_json::from_str(&original).unwrap();
    let mut panes: Value =
        serde_json::from_str(responses["pane list --workspace w1"].as_str().unwrap()).unwrap();
    panes["result"]["panes"][0]["terminal_id"] = json!("different-immutable-id");
    responses["pane list --workspace w1"] = json!(panes.to_string());
    fs::write(&rp, responses.to_string()).unwrap();
    assert_eq!(server.get(&retry).status, 410);
    assert_eq!(invocations(&f).len(), before);
    fs::write(&rp, &original).unwrap();
    drop(server);
    let server = Server::start(&f);
    assert_eq!(server.get(&retry).status, 410);
    f.db().execute("UPDATE agent_session_observations SET agent_session_id = 'replacement' WHERE address_id = ?1",[xws::address_id("w1","lead")]).unwrap();
    assert_eq!(server.get(&p).status, 410);
}

#[test]
fn initial_attach_revalidation_refuses_changed_terminal_before_paint() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let p = path("socket-a", "w1", "lead");
    let mut responses: Value =
        serde_json::from_str(&fs::read_to_string(f.root.join("responses.json")).unwrap()).unwrap();
    let mut panes: Value =
        serde_json::from_str(responses["pane list --workspace w1"].as_str().unwrap()).unwrap();
    panes["result"]["panes"][0]["terminal_id"] = json!("replacement");
    responses["pane list --workspace w1"] = json!(panes.to_string());
    configure(&f, json!({"mutate_metadata":responses}));
    let response = server.get(&p);
    assert_eq!(response.status, 410);
    assert!(!response.body.contains("SAFE-INITIAL"));
    assert_eq!(invocations(&f).len(), 1);
}

#[test]
fn missing_session_is_allowed_but_later_identity_changes_end_view() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let p = path("socket-a", "w1", "lead");
    f.db()
        .execute(
            "DELETE FROM agent_session_observations WHERE address_id = ?1",
            [xws::address_id("w1", "lead")],
        )
        .unwrap();
    let rp = f.root.join("responses.json");
    let mut responses: Value = serde_json::from_str(&fs::read_to_string(&rp).unwrap()).unwrap();
    let mut panes: Value =
        serde_json::from_str(responses["pane list --workspace w1"].as_str().unwrap()).unwrap();
    panes["result"]["panes"][0]
        .as_object_mut()
        .unwrap()
        .remove("agent_session");
    responses["pane list --workspace w1"] = json!(panes.to_string());
    fs::write(&rp, responses.to_string()).unwrap();
    configure(&f, json!({"quiet":true}));
    let mut live = server.live(&p);
    assert_eq!(live.status, 200);
    let reset = live.record().unwrap();
    assert_eq!(reset["kind"], "reset");
    panes["result"]["panes"][0]["agent_session"] = json!({"value":"newly-known"});
    responses["pane list --workspace w1"] = json!(panes.to_string());
    fs::write(&rp, responses.to_string()).unwrap();
    let final_record = live.record().unwrap();
    assert_eq!(final_record["kind"], "closed");
    assert_eq!(final_record["reason"], "replaced");
    let retry = format!("{p}?continuity={}", reset["continuity"].as_str().unwrap());
    assert_eq!(server.get(&retry).status, 410);
}

#[test]
fn logical_rebind_discards_live_target_and_retains_retry_continuity() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let p = path("socket-a", "w1", "lead");
    configure(&f, json!({"quiet":true}));
    let mut live = server.live(&p);
    let reset = live.record().unwrap();
    f.db()
        .execute(
            "UPDATE pane_bindings SET binding_id = 'new-binding' WHERE address_id = ?1",
            [xws::address_id("w1", "lead")],
        )
        .unwrap();
    assert_eq!(live.record().unwrap()["reason"], "replaced");
    assert_eq!(
        server
            .get(&format!(
                "{p}?continuity={}",
                reset["continuity"].as_str().unwrap()
            ))
            .status,
        410
    );
    assert_eq!(invocations(&f).len(), 1);
}

#[test]
fn eight_viewers_leave_other_routes_available_and_cancel_releases_capacity() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let p = path("socket-a", "w1", "lead");
    configure(&f, json!({"quiet":true,"child":true}));
    let mut clients = Vec::new();
    for _ in 0..8 {
        let mut live = server.live(&p);
        assert_eq!(live.status, 200);
        assert_eq!(live.record().unwrap()["kind"], "reset");
        clients.push(live);
    }
    assert_eq!(server.get(&p).status, 503);
    assert_eq!(server.get("/api/session").status, 200);
    drop(clients);
    let calls = invocations(&f);
    for call in calls {
        wait_for(|| stopped(call["pid"].as_u64().unwrap()));
        wait_for(|| stopped(call["child"].as_u64().unwrap()));
    }
    configure(&f, json!({}));
    assert_eq!(server.get(&p).status, 200);
}

#[test]
fn hard_dashboard_death_reaps_quiet_and_flooding_observer_groups() {
    for flood in [false, true] {
        let f = Fixture::new();
        let server = Server::start(&f);
        let p = path("socket-a", "w1", "lead");
        configure(&f, json!({"quiet":!flood,"flood":flood,"child":true}));
        let mut live = server.live(&p);
        assert_eq!(live.record().unwrap()["kind"], "reset");
        let call = invocations(&f).remove(0);
        drop(server);
        wait_for(|| stopped(call["pid"].as_u64().unwrap()));
        wait_for(|| stopped(call["child"].as_u64().unwrap()));
        drop(live);
    }
}

#[test]
fn slow_real_tcp_viewer_is_cancelled_without_stalling_other_viewers() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let p = path("socket-a", "w1", "lead");
    configure(&f, json!({"flood":true}));
    let mut slow = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    use std::os::fd::AsRawFd;
    let size: libc::c_int = 1024;
    // SAFETY: setsockopt borrows a valid socket and correctly sized integer.
    assert_eq!(
        unsafe {
            libc::setsockopt(
                slow.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVBUF,
                (&size as *const libc::c_int).cast(),
                std::mem::size_of_val(&size) as libc::socklen_t,
            )
        },
        0
    );
    write!(
        slow,
        "GET {p} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: {}\r\n\r\n",
        server.port, server.cookie
    )
    .unwrap();
    wait_for(|| !invocations(&f).is_empty());
    let pid = invocations(&f)[0]["pid"].as_u64().unwrap();
    configure(&f, json!({}));
    let healthy = server.get(&path("socket-a", "w1", "planner"));
    assert_eq!(healthy.status, 200);
    assert!(healthy.body.contains("reset"));
    wait_for(|| stopped(pid));
    drop(slow);
    assert_eq!(server.get("/api/session").status, 200);
}

#[test]
fn same_pane_and_address_on_another_socket_use_that_stored_namespace() {
    use namedrop::{
        ActorKind, MutationAttribution, PaneAddressDraft, PaneAddressId, PaneBindingId, PaneRoleId,
        StateStore,
    };
    let f = Fixture::new();
    let ns = StateNamespace::for_space("socket-b", "w1");
    let attribution = MutationAttribution::new(ActorKind::Human, "test", 43);
    StateStore::open(&f.state)
        .unwrap()
        .transaction(|tx| {
            tx.activate_worktree(&ns, &attribution)?;
            let draft = PaneAddressDraft {
                id: PaneAddressId::new(xws::address_id("w1", "lead")),
                role_id: PaneRoleId::new("other-role"),
                role_name: "lead".into(),
                role_description: String::new(),
                alias: "lead".into(),
            };
            tx.create_pane_address(&ns, &draft, &attribution)?;
            tx.bind_pane(
                &ns,
                &PaneBindingId::new("other-binding"),
                &draft.id,
                "pane-1-lead",
                &attribution,
            )
        })
        .unwrap();
    let server = Server::start(&f);
    assert_eq!(server.get(&path("socket-b", "w1", "lead")).status, 200);
    let call = invocations(&f).remove(0);
    assert_eq!(call["socket"], "socket-b");
    assert_eq!(call["args"][3], "t-pane-1-lead");
}
