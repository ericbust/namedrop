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
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{fs::PermissionsExt, process::CommandExt},
    },
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

fn metadata_with_rows(f: &Fixture, rows: Value) -> Value {
    let mut responses: Value =
        serde_json::from_slice(&fs::read(f.root.join("responses.json")).unwrap()).unwrap();
    let mut panes: Value =
        serde_json::from_str(responses["pane list --workspace w1"].as_str().unwrap()).unwrap();
    panes["result"]["panes"][0]["scroll"] = json!({"viewport_rows": rows});
    responses["pane list --workspace w1"] = json!(panes.to_string());
    responses
}

fn set_rows(f: &Fixture, rows: Value) {
    let temporary = f.root.join("responses-next.json");
    fs::write(&temporary, metadata_with_rows(f, rows).to_string()).unwrap();
    fs::rename(temporary, f.root.join("responses.json")).unwrap();
}

fn set_process_info(f: &Fixture, info: Value) {
    let mut responses: Value =
        serde_json::from_slice(&fs::read(f.root.join("responses.json")).unwrap()).unwrap();
    responses["pane process-info --pane pane-1-lead"] =
        json!(json!({"result":{"process_info":info}}).to_string());
    let temporary = f.root.join("responses-next.json");
    fs::write(&temporary, responses.to_string()).unwrap();
    fs::rename(temporary, f.root.join("responses.json")).unwrap();
}

struct SourcePty {
    master: fs::File,
    slave: fs::File,
    path: String,
}
impl SourcePty {
    fn new(cols: u16, rows: u16) -> Self {
        let (mut master, mut slave) = (-1, -1);
        let mut size = libc::winsize {
            ws_col: cols,
            ws_row: rows,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: openpty initializes both descriptors using the supplied size.
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    &raw mut size,
                )
            },
            0
        );
        // SAFETY: openpty returned two owned descriptors above.
        let master = unsafe { fs::File::from_raw_fd(master) };
        let slave = unsafe { fs::File::from_raw_fd(slave) };
        let mut name = [0; 512];
        // SAFETY: ttyname_r writes at most the supplied name buffer length.
        assert_eq!(
            unsafe { libc::ttyname_r(slave.as_raw_fd(), name.as_mut_ptr(), name.len()) },
            0
        );
        // SAFETY: ttyname_r succeeded and NUL terminated the buffer.
        let path = unsafe { std::ffi::CStr::from_ptr(name.as_ptr()) }
            .to_str()
            .unwrap()
            .to_owned();
        let mut attributes = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: tcgetattr initializes attributes for this disposable PTY.
        assert_eq!(
            unsafe { libc::tcgetattr(slave.as_raw_fd(), attributes.as_mut_ptr()) },
            0
        );
        // SAFETY: tcgetattr succeeded above.
        let mut attributes = unsafe { attributes.assume_init() };
        // SAFETY: these operations change only this test-owned PTY.
        unsafe {
            libc::cfmakeraw(&mut attributes);
            assert_eq!(
                libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &attributes),
                0
            );
        }
        // SAFETY: nonblocking reads make a consumed-input regression fail instead of hanging.
        unsafe {
            let flags = libc::fcntl(slave.as_raw_fd(), libc::F_GETFL);
            assert!(flags >= 0);
            assert_eq!(
                libc::fcntl(slave.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK),
                0
            );
        }
        Self {
            master,
            slave,
            path,
        }
    }
    fn resize(&self, cols: u16, rows: u16) {
        let size = libc::winsize {
            ws_col: cols,
            ws_row: rows,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: only this disposable source is resized by the test, never by the dashboard.
        assert_eq!(
            unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &size) },
            0
        );
    }
    fn size(&self) -> (u16, u16) {
        let mut size = std::mem::MaybeUninit::<libc::winsize>::uninit();
        // SAFETY: ioctl initializes size for the owned descriptor.
        assert_eq!(
            unsafe { libc::ioctl(self.slave.as_raw_fd(), libc::TIOCGWINSZ, size.as_mut_ptr()) },
            0
        );
        // SAFETY: TIOCGWINSZ succeeded above.
        let size = unsafe { size.assume_init() };
        (size.ws_col, size.ws_row)
    }
}

fn configure_history(f: &Fixture, text: &str) {
    fs::write(
        f.root.join("history.json"),
        serde_json::to_vec(text).unwrap(),
    )
    .unwrap();
}

fn history_invocations(f: &Fixture) -> Vec<Value> {
    fs::read_to_string(f.root.join("history-reads.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn invocations(f: &Fixture) -> Vec<Value> {
    fs::read_to_string(f.root.join("reads.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn geometry_invocations(f: &Fixture) -> Vec<Value> {
    fs::read_to_string(f.root.join("geometry-reads.jsonl"))
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
        configure_history(f, "");
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
    if config.get('reject_geometry') and ('--rows' in args or '--cols' in args):
        time.sleep(config.get('reject_delay',0))
        os.write(1,config.get('reject_stdout','').encode());sys.exit(2)
    if config.get('exit') is not None:
        os.write(1,config.get('stdout','').encode())
        sys.exit(config['exit'])
    rows=int(args[args.index('--rows')+1]) if '--rows' in args else 40
    cols=int(args[args.index('--cols')+1]) if '--cols' in args else 120
    if config.get('gate_rows') and rows in config['gate_rows']:
        while True:time.sleep(.01)
    if config.get('gate'):
        while not os.path.exists(os.path.join(root,config['gate'])):time.sleep(.01)
    time.sleep(config.get('delay_rows',{}).get(str(rows),0))
    if config.get('mutate_metadata'):
        json.dump(config['mutate_metadata'],open(os.path.join(root,'responses.json'),'w'))
    if config.get('raw') is not None:
        os.write(1,config['raw'].encode());sys.exit(0)
    if config.get('oversize'):
        os.write(1,b'x'*(2*1024*1024+1));time.sleep(120)
    payload=b'\x1b[2J\x1b[HSAFE-INITIAL'
    if config.get('source_rows') and rows>=60:payload+=b'\x1b[60;1HBOTTOM-PROMPT'
    if config.get('source_cols') and cols>=200 and rows>=70:payload+=b'\x1b[70;190HRIGHT-EDGE'
    frames=config.get('frames',[{'type':'terminal.frame','encoding':'ansi','full':True,'width':cols,'height':rows,'seq':1,'bytes':base64.b64encode(payload).decode()}])
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
if args[:2]==['pane','read']:
    if '--lines' in args and args[args.index('--lines')+1]=='2200':
        with open(os.path.join(root,'history-reads.jsonl'),'a') as log:
            log.write(json.dumps({'args':args,'socket':os.environ.get('HERDR_SOCKET_PATH'),'pane':os.environ.get('HERDR_PANE_ID')})+'\n')
        config=json.load(open(os.path.join(root,'observer.json')))
        if config.get('history_mutate_metadata'):
            json.dump(config['history_mutate_metadata'],open(os.path.join(root,'responses.json'),'w'))
        os.write(1,json.load(open(os.path.join(root,'history.json'))).encode());sys.exit(0)
    print('SANITIZED SNAPSHOT');sys.exit(0)
if args[:2] in [['pane','send-text'],['pane','send-keys']]:
    with open(os.path.join(root,'reads.jsonl'),'a') as log:
        log.write(json.dumps({'args':args,'socket':os.environ.get('HERDR_SOCKET_PATH')})+'\n')
    sys.exit(0)
if args[:2]==['pane','process-info']:
    with open(os.path.join(root,'geometry-reads.jsonl'),'a') as log:
        log.write(json.dumps({'args':args,'time':time.monotonic()})+'\n')
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

    fn post(&self, path: &str, body: &str, headers: &[(&str, &str)]) -> Response {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(stream, "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n", self.port, body.len()).unwrap();
        for (name, value) in headers {
            write!(stream, "{name}: {value}\r\n").unwrap();
        }
        write!(stream, "\r\n{body}").unwrap();
        read_response(stream)
    }

    fn csrf(&self) -> String {
        serde_json::from_str::<Value>(&self.get("/api/session").body).unwrap()["csrf_token"]
            .as_str()
            .unwrap()
            .to_owned()
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

fn input_path(socket: &str, workspace: &str, alias: &str) -> String {
    path(socket, workspace, alias).replace("/live", "/input")
}

fn live_continuity(server: &Server, target: &str) -> String {
    let response = server.get(target);
    assert_eq!(response.status, 200);
    serde_json::from_str::<Value>(response.body.lines().next().unwrap()).unwrap()["continuity"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn post_input(server: &Server, target: &str, body: &Value) -> Response {
    let csrf = server.csrf();
    server.post(
        target,
        &body.to_string(),
        &[("Cookie", &server.cookie), ("X-Namedrop-Csrf", &csrf)],
    )
}

#[test]
fn source_tty_preserves_wide_bottom_marker_without_consuming_source_input() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let mut source = SourcePty::new(200, 80);
    set_rows(&f, json!(40));
    set_process_info(
        &f,
        json!({"tty":source.path, "shell_pid":"ignored-when-tty-is-present"}),
    );
    configure(&f, json!({"source_cols":true}));
    source
        .master
        .write_all(b"source-input-still-pending")
        .unwrap();
    let response = server.get(&path("socket-a", "w1", "lead"));
    assert_eq!(response.status, 200);
    let reset: Value = serde_json::from_str(response.body.lines().next().unwrap()).unwrap();
    assert_eq!(
        (reset["cols"].as_u64(), reset["rows"].as_u64()),
        (Some(200), Some(80))
    );
    assert_eq!(
        reset["bytes"],
        "G1syShtbSFNBRkUtSU5JVElBTBtbNzA7MTkwSFJJR0hULUVER0U="
    );
    assert_eq!(source.size(), (200, 80));
    let mut pending = [0; 26];
    source.slave.read_exact(&mut pending).unwrap();
    assert_eq!(&pending, b"source-input-still-pending");
    let call = invocations(&f).remove(0);
    assert_eq!(
        call["args"],
        json!([
            "terminal",
            "session",
            "observe",
            "t-pane-1-lead",
            "--rows",
            "80",
            "--cols",
            "200"
        ])
    );
    assert_eq!(call["stdin_null"], true);
}

#[test]
fn source_column_changes_keep_continuity_sequence_history_and_input() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let source = SourcePty::new(200, 80);
    set_process_info(&f, json!({"tty":source.path}));
    configure(&f, json!({"quiet":true,"child":true,"source_cols":true}));
    configure_history(&f, "history before columns change");
    let mut live = server.live(&path("socket-a", "w1", "lead"));
    let reset = live.record().unwrap();
    let continuity = reset["continuity"].clone();
    assert_eq!(reset["cols"], 200);
    assert_eq!(live.record().unwrap()["kind"], "history");
    let mut seq = 2;
    for cols in [240, 100, 200] {
        source.resize(cols, 80);
        let event = live.record().unwrap();
        assert_eq!(event["kind"], "reset");
        assert_eq!(event["cols"], cols);
        assert_eq!(event["rows"], 80);
        assert_eq!(event["continuity"], continuity);
        seq += 1;
        assert_eq!(event["seq"], seq);
        assert_eq!(
            post_input(
                &server,
                &input_path("socket-a", "w1", "lead"),
                &json!({"kind":"key","key":"Enter","continuity":continuity})
            )
            .status,
            204
        );
    }
    configure_history(&f, "history after columns change");
    let history = live.record().unwrap();
    assert_eq!(history["kind"], "history");
    assert_eq!(history["text"], "history after columns change");
    assert_eq!(history["seq"], seq + 1);
    drop(live);
    let calls: Vec<_> = invocations(&f)
        .into_iter()
        .filter(|call| call["args"][0] == "terminal")
        .collect();
    assert_eq!(calls.len(), 4);
    assert_eq!(geometry_invocations(&f).len(), 1);
    for call in calls {
        wait_for(|| stopped(call["pid"].as_u64().unwrap()));
        wait_for(|| stopped(call["child"].as_u64().unwrap()));
    }
}

#[test]
fn unavailable_geometry_keeps_known_columns_until_authoritative_resize() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let source = SourcePty::new(200, 80);
    set_rows(&f, json!(80));
    set_process_info(&f, json!({"tty":source.path}));
    configure(&f, json!({"quiet":true,"source_cols":true}));
    configure_history(&f, "geometry available");
    let mut live = server.live(&path("socket-a", "w1", "lead"));
    let reset = live.record().unwrap();
    assert_eq!(reset["cols"], 200);
    assert_eq!(live.record().unwrap()["kind"], "history");
    for (index, info) in [json!({}), json!({"tty":"/dev/namedrop-test-missing-tty"})]
        .into_iter()
        .enumerate()
    {
        set_process_info(&f, info);
        let text = format!("geometry temporarily unavailable {index}");
        configure_history(&f, &text);
        let event = live.record().unwrap();
        assert_eq!(
            event["kind"], "history",
            "optional geometry loss reset the known source grid"
        );
        assert_eq!(event["text"], text);
        assert_eq!(invocations(&f).len(), 1);
    }
    source.resize(240, 80);
    set_process_info(&f, json!({"tty":source.path}));
    let resized = live.record().unwrap();
    assert_eq!(resized["kind"], "reset");
    assert_eq!(resized["cols"], 240);
    assert_eq!(resized["continuity"], reset["continuity"]);
    assert_eq!(resized["seq"], 5);
    assert_eq!(invocations(&f).len(), 2);
}

#[test]
fn partial_geometry_revalidates_retained_columns_against_new_rows() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let source = SourcePty::new(512, 100);
    set_process_info(&f, json!({"tty":source.path}));
    configure(&f, json!({"quiet":true}));
    let mut live = server.live(&path("socket-a", "w1", "lead"));
    let reset = live.record().unwrap();
    assert_eq!(reset["cols"], 512);
    assert_eq!(reset["rows"], 100);
    source.resize(0, 0);
    set_rows(&f, json!(256));
    set_process_info(&f, json!({}));
    let event = live.record().unwrap();
    assert_eq!(event["kind"], "reset");
    assert_eq!(event["cols"], 512);
    assert_eq!(event["rows"], 128);
    assert_eq!(event["continuity"], reset["continuity"]);
    assert_eq!(invocations(&f).len(), 2);
    assert_eq!(
        post_input(
            &server,
            &input_path("socket-a", "w1", "lead"),
            &json!({"kind":"key","key":"Enter","continuity":reset["continuity"]})
        )
        .status,
        204
    );
}

#[test]
fn invalid_source_geometry_falls_back_or_clamps_without_revoking_input() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let source = SourcePty::new(200, 80);
    let p = path("socket-a", "w1", "lead");
    let continuity = live_continuity(&server, &p);
    set_process_info(&f, json!({"tty":source.path}));
    set_rows(&f, json!(60));
    for (cols, rows, expected) in [
        (0, 0, (120, 60)),
        (0, 80, (120, 60)),
        (513, 80, (512, 80)),
        (200, 0, (120, 60)),
        (200, 257, (200, 256)),
        (512, 256, (512, 128)),
        (600, 300, (512, 128)),
    ] {
        source.resize(cols, rows);
        let response = server.get(&p);
        assert_eq!(response.status, 200, "source {cols}x{rows}");
        let reset: Value = serde_json::from_str(response.body.lines().next().unwrap()).unwrap();
        assert_eq!(
            (reset["cols"].as_u64(), reset["rows"].as_u64()),
            (Some(expected.0), Some(expected.1))
        );
        assert_eq!(reset["continuity"], continuity);
    }
    for info in [
        json!({"tty":3}),
        json!({"tty":"??"}),
        json!({"tty":"console"}),
        json!({"tty":" "}),
        json!({"tty":"pts/1 extra"}),
        json!({"tty":"/tmp/untrusted"}),
        json!({"tty":"/dev/pts/../0"}),
        json!({"shell_pid":"bad"}),
        json!({"shell_pid":0}),
        json!({"shell_pid":u64::MAX}),
    ] {
        set_process_info(&f, info);
        let response = server.get(&p);
        assert_eq!(response.status, 200);
        let reset: Value = serde_json::from_str(response.body.lines().next().unwrap()).unwrap();
        assert_eq!(reset["rows"], 60);
        assert_eq!(reset["continuity"], continuity);
    }
    for rows in [
        json!("bad"),
        json!(null),
        json!(-1),
        json!(0),
        json!({}),
        json!([]),
    ] {
        set_rows(&f, rows);
        let response = server.get(&p);
        assert_eq!(response.status, 200);
        let reset: Value = serde_json::from_str(response.body.lines().next().unwrap()).unwrap();
        assert_eq!(reset["rows"], 40);
        assert_eq!(reset["continuity"], continuity);
    }
    assert_eq!(
        post_input(
            &server,
            &input_path("socket-a", "w1", "lead"),
            &json!({"kind":"key","key":"Enter","continuity":continuity})
        )
        .status,
        204
    );
}

#[test]
fn unavailable_tty_keeps_legacy_rows_and_absent_metadata_keeps_defaults() {
    let f = Fixture::new();
    let server = Server::start(&f);
    set_process_info(&f, json!({"tty":"/dev/namedrop-test-missing-tty"}));
    let p = path("socket-a", "w1", "lead");
    let response = server.get(&p);
    let reset: Value = serde_json::from_str(response.body.lines().next().unwrap()).unwrap();
    assert_eq!(reset["cols"], 120);
    assert_eq!(reset["rows"], 40);
    assert_eq!(
        invocations(&f)[0]["args"],
        json!(["terminal", "session", "observe", "t-pane-1-lead"])
    );
    set_rows(&f, json!(80));
    let response = server.get(&p);
    let reset: Value = serde_json::from_str(response.body.lines().next().unwrap()).unwrap();
    assert_eq!(reset["cols"], 120);
    assert_eq!(reset["rows"], 80);
}

#[test]
fn shell_pid_discovers_controlling_tty_with_bounded_ps() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let source = SourcePty::new(200, 70);
    let mut command = Command::new("/usr/bin/python3");
    command
        .args(["-c", "import time;time.sleep(120)"])
        .stdin(source.slave.try_clone().unwrap())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: the child creates its own session and claims only the disposable test PTY.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut shell = command.spawn().unwrap();
    set_process_info(&f, json!({"shell_pid":shell.id()}));
    let started = Instant::now();
    let response = server.get(&path("socket-a", "w1", "lead"));
    let _ = shell.kill();
    let _ = shell.wait();
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(response.status, 200);
    let reset: Value = serde_json::from_str(response.body.lines().next().unwrap()).unwrap();
    assert_eq!(reset["cols"], 200);
    assert_eq!(reset["rows"], 70);
    assert_eq!(source.size(), (200, 70));
    assert_eq!(geometry_invocations(&f).len(), 1);
}

#[test]
fn cached_tty_rechecks_startup_resize_without_rediscovery_or_retained_descriptor() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let source = SourcePty::new(200, 80);
    set_process_info(&f, json!({"tty":source.path}));
    configure(&f, json!({"delay_rows":{"80":0.3},"quiet":true}));
    thread::scope(|scope| {
        scope.spawn(|| {
            wait_for(|| !invocations(&f).is_empty());
            source.resize(240, 80);
        });
        let mut live = server.live(&path("socket-a", "w1", "lead"));
        let reset = live.record().unwrap();
        assert_eq!(reset["cols"], 240);
        assert_eq!(geometry_invocations(&f).len(), 1);
        assert_eq!(invocations(&f).len(), 2);
        #[cfg(target_os = "linux")]
        for descriptor in fs::read_dir(format!("/proc/{}/fd", server.child.id())).unwrap() {
            let target = fs::read_link(descriptor.unwrap().path()).unwrap();
            assert_ne!(target, std::path::Path::new(&source.path));
        }
    });
}

#[test]
fn failed_tty_discovery_throttles_startup_but_rechecks_metadata_rows() {
    let f = Fixture::new();
    let server = Server::start(&f);
    set_rows(&f, json!(40));
    set_process_info(&f, json!({"tty":"??"}));
    configure(
        &f,
        json!({"mutate_metadata":metadata_with_rows(&f, json!(80))}),
    );
    let response = server.get(&path("socket-a", "w1", "lead"));
    assert_eq!(response.status, 200);
    let reset: Value = serde_json::from_str(response.body.lines().next().unwrap()).unwrap();
    assert_eq!(reset["rows"], 80);
    assert_eq!(geometry_invocations(&f).len(), 1);
    assert_eq!(invocations(&f).len(), 2);
}

#[test]
fn legacy_retry_preserves_successful_tty_discovery_across_resizes() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let source = SourcePty::new(200, 80);
    set_process_info(&f, json!({"tty":source.path}));
    configure(&f, json!({"reject_geometry":true,"quiet":true}));
    let mut live = server.live(&path("socket-a", "w1", "lead"));
    let reset = live.record().unwrap();
    assert_eq!(reset["cols"], 120);
    source.resize(240, 80);
    let resized = live.record().unwrap();
    assert_eq!(resized["kind"], "reset");
    assert_eq!(resized["continuity"], reset["continuity"]);
    assert_eq!(invocations(&f).len(), 4);
    assert_eq!(geometry_invocations(&f).len(), 1);
}

#[test]
fn legacy_observer_retries_without_geometry_once_at_startup_and_resize() {
    let f = Fixture::new();
    let server = Server::start(&f);
    set_rows(&f, json!(80));
    configure(
        &f,
        json!({"reject_geometry":true,"quiet":true,"child":true}),
    );
    configure_history(&f, "legacy observer history");
    let mut live = server.live(&path("socket-a", "w1", "lead"));
    let reset = live.record().unwrap();
    assert_eq!(reset["rows"], 40);
    assert_eq!(reset["seq"], 1);
    assert_eq!(live.record().unwrap()["kind"], "history");
    set_rows(&f, json!(60));
    let resized = live.record().unwrap();
    assert_eq!(resized["kind"], "reset");
    assert_eq!(resized["seq"], 3);
    assert_eq!(resized["continuity"], reset["continuity"]);
    assert_eq!(resized["rows"], 40);
    assert_eq!(
        post_input(
            &server,
            &input_path("socket-a", "w1", "lead"),
            &json!({"kind":"key","key":"Enter","continuity":reset["continuity"]})
        )
        .status,
        204
    );
    drop(live);
    let calls: Vec<_> = invocations(&f)
        .into_iter()
        .filter(|call| call["args"][0] == "terminal")
        .collect();
    assert_eq!(calls.len(), 4);
    assert_eq!(calls[0]["args"].as_array().unwrap().len(), 6);
    assert_eq!(calls[1]["args"].as_array().unwrap().len(), 4);
    assert_eq!(calls[2]["args"].as_array().unwrap().len(), 6);
    assert_eq!(calls[3]["args"].as_array().unwrap().len(), 4);
    for call in calls {
        wait_for(|| stopped(call["pid"].as_u64().unwrap()));
        wait_for(|| stopped(call["child"].as_u64().unwrap()));
    }
}

#[test]
fn geometry_usage_retry_requires_no_stdout_and_second_failure_is_unsupported() {
    let f = Fixture::new();
    let server = Server::start(&f);
    set_rows(&f, json!(80));
    let p = path("socket-a", "w1", "lead");
    for stdout in [" ", "{\"type\":", "{bad\n"] {
        configure(&f, json!({"reject_geometry":true,"reject_stdout":stdout}));
        let before = invocations(&f).len();
        let response = server.get(&p);
        assert_eq!(invocations(&f).len(), before + 1);
        assert!(!response.body.contains("unsupported"));
    }
    configure(&f, json!({"reject_geometry":true,"exit":2}));
    let before = invocations(&f).len();
    assert!(server.get(&p).body.contains("unsupported"));
    assert_eq!(invocations(&f).len(), before + 2);
    configure(&f, json!({"exit":1}));
    let before = invocations(&f).len();
    assert_eq!(server.get(&p).status, 503);
    assert_eq!(invocations(&f).len(), before + 1);
}

#[test]
fn flagless_retry_keeps_original_deadline_and_cleans_children() {
    let f = Fixture::new();
    let server = Server::start(&f);
    set_rows(&f, json!(80));
    configure(
        &f,
        json!({"reject_geometry":true,"reject_delay":2,"gate":"never","child":true}),
    );
    let started = Instant::now();
    assert_eq!(server.get(&path("socket-a", "w1", "lead")).status, 503);
    assert!(started.elapsed() < Duration::from_secs(6));
    let calls = invocations(&f);
    assert_eq!(calls.len(), 2);
    for call in calls {
        wait_for(|| stopped(call["pid"].as_u64().unwrap()));
        wait_for(|| stopped(call["child"].as_u64().unwrap()));
    }
}

#[test]
fn source_rows_include_bottom_prompt_and_keep_observation_passive() {
    let f = Fixture::new();
    let server = Server::start(&f);
    set_rows(&f, json!(60));
    configure(&f, json!({"source_rows":true}));
    let response = server.get(&path("socket-a", "w1", "lead"));
    assert_eq!(response.status, 200);
    let reset: Value = serde_json::from_str(response.body.lines().next().unwrap()).unwrap();
    assert_eq!(reset["rows"], 60);
    assert_eq!(reset["cols"], 120);
    assert_eq!(
        reset["bytes"],
        "G1syShtbSFNBRkUtSU5JVElBTBtbNjA7MUhCT1RUT00tUFJPTVBU"
    );
    let call = invocations(&f).remove(0);
    assert_eq!(
        call["args"],
        json!([
            "terminal",
            "session",
            "observe",
            "t-pane-1-lead",
            "--rows",
            "60"
        ])
    );
    assert_eq!(call["stdin_null"], true);
    assert!(call["pane"].is_null() && call["workspace"].is_null());
}

#[test]
fn same_identity_row_changes_preserve_stream_continuity_input_and_history() {
    let f = Fixture::new();
    let server = Server::start(&f);
    set_rows(&f, json!(40));
    configure(&f, json!({"quiet":true,"source_rows":true,"child":true}));
    configure_history(&f, "earlier history\n");
    let mut live = server.live(&path("socket-a", "w1", "lead"));
    let reset = live.record().unwrap();
    let continuity = reset["continuity"].clone();
    let initial_history = live.record().unwrap();
    assert_eq!(initial_history["kind"], "history");
    assert_eq!(initial_history["seq"], 2);
    let mut seq = 2;
    for rows in [60, 20] {
        set_rows(&f, json!(rows));
        let event = live.record().unwrap();
        assert_eq!(event["kind"], "reset");
        assert_eq!(event["rows"], rows);
        assert_eq!(event["continuity"], continuity);
        seq += 1;
        assert_eq!(event["seq"], seq);
        assert_eq!(
            post_input(
                &server,
                &input_path("socket-a", "w1", "lead"),
                &json!({"kind":"key","key":"Enter","continuity":continuity})
            )
            .status,
            204
        );
    }
    configure_history(&f, "history survives both resizes\n");
    let event = live.record().unwrap();
    assert_eq!(event["kind"], "history");
    assert_eq!(event["seq"], seq + 1);
    assert_eq!(event["text"], "history survives both resizes");
    let calls: Vec<_> = invocations(&f)
        .into_iter()
        .filter(|call| call["args"][0] == "terminal")
        .collect();
    assert_eq!(calls.len(), 3);
    for call in &calls[..2] {
        wait_for(|| stopped(call["pid"].as_u64().unwrap()));
        wait_for(|| stopped(call["child"].as_u64().unwrap()));
    }
    drop(live);
    wait_for(|| stopped(calls[2]["pid"].as_u64().unwrap()));
    wait_for(|| stopped(calls[2]["child"].as_u64().unwrap()));
}

#[test]
fn first_paint_rechecks_rows_and_retries_before_http_headers() {
    let f = Fixture::new();
    let server = Server::start(&f);
    set_rows(&f, json!(40));
    let changed = metadata_with_rows(&f, json!(60));
    configure(&f, json!({"source_rows":true,"mutate_metadata":changed}));
    let response = server.get(&path("socket-a", "w1", "lead"));
    assert_eq!(response.status, 200);
    let reset: Value = serde_json::from_str(response.body.lines().next().unwrap()).unwrap();
    assert_eq!(reset["rows"], 60);
    assert_eq!(reset["seq"], 1);
    assert_eq!(invocations(&f).len(), 2);
}

#[test]
fn initial_row_retry_keeps_original_startup_deadline() {
    let f = Fixture::new();
    let server = Server::start(&f);
    set_rows(&f, json!(40));
    let changed = metadata_with_rows(&f, json!(60));
    configure(
        &f,
        json!({"mutate_metadata":changed,"gate_rows":[60],"delay_rows":{"40":2},"child":true}),
    );
    let started = Instant::now();
    assert_eq!(server.get(&path("socket-a", "w1", "lead")).status, 503);
    assert!(started.elapsed() < Duration::from_secs(7));
    assert_eq!(invocations(&f).len(), 2);
    for call in invocations(&f) {
        wait_for(|| stopped(call["pid"].as_u64().unwrap()));
        wait_for(|| stopped(call["child"].as_u64().unwrap()));
    }
}

#[test]
fn invalid_source_rows_fall_back_or_clamp_without_revoking_input_continuity() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let p = path("socket-a", "w1", "lead");
    let continuity = live_continuity(&server, &p);
    let before = invocations(&f).len();
    for (rows, expected) in [
        (json!(0), 40),
        (json!(-1), 40),
        (json!(257), 256),
        (json!("60"), 40),
        (json!(60.5), 40),
        (Value::Null, 40),
    ] {
        set_rows(&f, rows);
        let response = server.get(&p);
        assert_eq!(response.status, 200);
        let reset: Value = serde_json::from_str(response.body.lines().next().unwrap()).unwrap();
        assert_eq!(reset["rows"], expected);
        assert_eq!(reset["continuity"], continuity);
    }
    assert_eq!(invocations(&f).len(), before + 6);
    assert_eq!(
        post_input(
            &server,
            &input_path("socket-a", "w1", "lead"),
            &json!({"kind":"key","key":"Enter","continuity":continuity})
        )
        .status,
        204
    );
}

#[test]
fn malformed_reobservation_ends_stream_and_reaps_both_observer_groups() {
    for config in [
        json!({"raw":"{broken\n","child":true}),
        json!({"frames":[{"type":"terminal.frame","encoding":"ansi","full":false,"width":120,"height":60,"seq":1,"bytes":"eA=="}],"child":true}),
    ] {
        let f = Fixture::new();
        let server = Server::start(&f);
        set_rows(&f, json!(40));
        configure(&f, json!({"quiet":true,"child":true}));
        let mut live = server.live(&path("socket-a", "w1", "lead"));
        assert_eq!(live.record().unwrap()["kind"], "reset");
        configure(&f, config);
        set_rows(&f, json!(60));
        let event = live.record().unwrap();
        assert_eq!(
            event,
            json!({"v":1,"seq":2,"kind":"unavailable","reason":"invalid_source"})
        );
        assert_eq!(invocations(&f).len(), 2);
        for call in invocations(&f) {
            wait_for(|| stopped(call["pid"].as_u64().unwrap()));
            wait_for(|| stopped(call["child"].as_u64().unwrap()));
        }
    }
}

#[test]
fn identity_replacement_during_reobservation_closes_original_continuity() {
    let f = Fixture::new();
    let server = Server::start(&f);
    set_rows(&f, json!(40));
    configure(&f, json!({"quiet":true,"gate_rows":[60],"child":true}));
    let p = path("socket-a", "w1", "lead");
    let mut live = server.live(&p);
    let reset = live.record().unwrap();
    set_rows(&f, json!(60));
    wait_for(|| invocations(&f).len() == 2);
    let mut responses = metadata_with_rows(&f, json!(60));
    let mut panes: Value =
        serde_json::from_str(responses["pane list --workspace w1"].as_str().unwrap()).unwrap();
    panes["result"]["panes"][0]["terminal_id"] = json!("replacement");
    responses["pane list --workspace w1"] = json!(panes.to_string());
    fs::write(f.root.join("responses.json"), responses.to_string()).unwrap();
    assert_eq!(
        live.record().unwrap(),
        json!({"v":1,"seq":2,"kind":"closed","reason":"replaced"})
    );
    assert_eq!(
        server
            .get(&format!(
                "{p}?continuity={}",
                reset["continuity"].as_str().unwrap()
            ))
            .status,
        410
    );
    assert_eq!(invocations(&f).len(), 2);
    for call in invocations(&f) {
        wait_for(|| stopped(call["pid"].as_u64().unwrap()));
        wait_for(|| stopped(call["child"].as_u64().unwrap()));
    }
}

#[test]
fn quiet_reobservations_share_one_first_paint_deadline() {
    let f = Fixture::new();
    let server = Server::start(&f);
    set_rows(&f, json!(40));
    configure(
        &f,
        json!({"quiet":true,"gate_rows":[60,30,20],"child":true}),
    );
    let mut live = server.live(&path("socket-a", "w1", "lead"));
    let reset = live.record().unwrap();
    let started = Instant::now();
    for (index, rows) in [60, 30, 20].into_iter().enumerate() {
        set_rows(&f, json!(rows));
        wait_for(|| invocations(&f).len() >= index + 2);
    }
    let event = live.record().unwrap();
    assert_eq!(event["kind"], "unavailable");
    assert_eq!(event["reason"], "source_lost");
    assert_eq!(event["seq"], reset["seq"].as_u64().unwrap() + 1);
    assert!(started.elapsed() < Duration::from_secs(8));
    for call in invocations(&f) {
        wait_for(|| stopped(call["pid"].as_u64().unwrap()));
        wait_for(|| stopped(call["child"].as_u64().unwrap()));
    }
}

#[test]
fn input_requires_session_and_csrf_before_any_send() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let live = path("socket-a", "w1", "lead");
    let target = input_path("socket-a", "w1", "lead");
    let body =
        json!({"kind":"key","key":"Enter","continuity":live_continuity(&server,&live)}).to_string();
    assert_eq!(server.post(&target, &body, &[]).status, 401);
    assert_eq!(
        server
            .post(&target, &body, &[("Cookie", &server.cookie)])
            .status,
        403
    );
    assert!(
        !invocations(&f)
            .iter()
            .any(|call| call["args"][1] == "send-keys")
    );
}

#[test]
fn input_sends_text_then_enter_and_allowlisted_keys() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let live = path("socket-a", "w1", "lead");
    let target = input_path("socket-a", "w1", "lead");
    let continuity = live_continuity(&server, &live);
    for text in ["hello agent", "--help  preserve 'quotes' and café"] {
        assert_eq!(
            post_input(
                &server,
                &target,
                &json!({"kind":"text","text":text,"continuity":continuity})
            )
            .status,
            204
        );
    }
    assert_eq!(
        post_input(
            &server,
            &target,
            &json!({"kind":"key","key":"Escape","continuity":continuity})
        )
        .status,
        204
    );
    let sends: Vec<_> = invocations(&f)
        .into_iter()
        .filter(|call| call["args"][1] == "send-text" || call["args"][1] == "send-keys")
        .collect();
    assert_eq!(
        sends
            .iter()
            .map(|call| call["args"].clone())
            .collect::<Vec<_>>(),
        vec![
            json!(["pane", "send-text", "pane-1-lead", "hello agent"]),
            json!(["pane", "send-keys", "pane-1-lead", "enter"]),
            json!([
                "pane",
                "send-text",
                "pane-1-lead",
                "--help  preserve 'quotes' and café"
            ]),
            json!(["pane", "send-keys", "pane-1-lead", "enter"]),
            json!(["pane", "send-keys", "pane-1-lead", "esc"]),
        ]
    );
    assert!(sends.iter().all(|call| call["socket"] == "socket-a"));
}

#[test]
fn input_refuses_wrong_binding_changed_session_and_offline_pane() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let live = path("socket-a", "w1", "lead");
    let target = input_path("socket-a", "w1", "lead");
    let continuity = live_continuity(&server, &live);
    let body = json!({"kind":"key","key":"Enter","continuity":continuity});
    assert_eq!(
        post_input(&server, &input_path("socket-a", "w1", "planner"), &body).status,
        410
    );
    f.db()
        .execute(
            "UPDATE pane_bindings SET pane_id = 'other-pane' WHERE pane_id = 'pane-1-lead'",
            [],
        )
        .unwrap();
    assert_eq!(post_input(&server, &target, &body).status, 410);
    f.db()
        .execute(
            "UPDATE pane_bindings SET pane_id = 'pane-1-lead' WHERE pane_id = 'other-pane'",
            [],
        )
        .unwrap();
    f.db().execute("UPDATE agent_session_observations SET agent_session_id = 'different' WHERE address_id = ?1", [xws::address_id("w1", "lead")]).unwrap();
    assert_eq!(post_input(&server, &target, &body).status, 410);
    f.db().execute("UPDATE agent_session_observations SET agent_session_id = 'session-pane-1-lead' WHERE address_id = ?1", [xws::address_id("w1", "lead")]).unwrap();
    let rp = f.root.join("responses.json");
    let mut responses: Value = serde_json::from_str(&fs::read_to_string(&rp).unwrap()).unwrap();
    let mut panes: Value =
        serde_json::from_str(responses["pane list --workspace w1"].as_str().unwrap()).unwrap();
    panes["result"]["panes"][0]["agent_session"] = json!({"value":"changed-native-session"});
    responses["pane list --workspace w1"] = json!(panes.to_string());
    fs::write(&rp, responses.to_string()).unwrap();
    assert_eq!(post_input(&server, &target, &body).status, 410);
    panes["result"]["panes"][0]
        .as_object_mut()
        .unwrap()
        .remove("agent_session");
    responses["pane list --workspace w1"] = json!(panes.to_string());
    fs::write(&rp, responses.to_string()).unwrap();
    assert_eq!(post_input(&server, &target, &body).status, 410);
    panes["result"]["panes"][0]["agent_session"] = json!({"value":"session-pane-1-lead"});
    panes["result"]["panes"][0]["agent_status"] = json!("offline");
    responses["pane list --workspace w1"] = json!(panes.to_string());
    fs::write(&rp, responses.to_string()).unwrap();
    assert_eq!(post_input(&server, &target, &body).status, 410);
    assert!(
        !invocations(&f)
            .iter()
            .any(|call| call["args"][1] == "send-keys")
    );
}

#[test]
fn input_rejects_oversize_control_text_and_unknown_keys() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let continuity = live_continuity(&server, &path("socket-a", "w1", "lead"));
    let target = input_path("socket-a", "w1", "lead");
    for body in [
        json!({"kind":"key","key":"F1","continuity":continuity}),
        json!({"kind":"text","text":"hello\nworld","continuity":continuity}),
    ] {
        assert_eq!(post_input(&server, &target, &body).status, 400);
    }
    let big = json!({"kind":"text","text":"x".repeat(8192),"continuity":continuity});
    assert_eq!(post_input(&server, &target, &big).status, 413);
    assert!(
        !invocations(&f)
            .iter()
            .any(|call| call["args"][1] == "send-text" || call["args"][1] == "send-keys")
    );
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
fn recent_history_is_bounded_sanitized_and_sent_after_reset() {
    let f = Fixture::new();
    let server = Server::start(&f);
    configure(&f, json!({"quiet":true}));
    let history = (0..2300)
        .map(|i| format!("history-{i:04}\n"))
        .collect::<String>()
        + "\u{1b}[31mSECRET\u{1b}[0m\0tail\n";
    configure_history(&f, &history);
    let mut live = server.live(&path("socket-a", "w1", "lead"));
    assert_eq!(live.status, 200);
    assert_eq!(live.record().unwrap()["kind"], "reset");
    let event = live.record().unwrap();
    assert_eq!(event["kind"], "history");
    assert_eq!(event["seq"], 2);
    let text = event["text"].as_str().unwrap();
    assert!(!text.contains("history-0000"));
    assert!(text.contains("history-0101"));
    assert!(text.contains("SECRETtail"));
    assert!(!text.contains('\u{1b}') && !text.contains('\0'));
    assert!(text.len() <= 1024 * 1024);
    assert!(text.lines().count() <= 2200);
    let calls = history_invocations(&f);
    assert!(!calls.is_empty());
    assert_eq!(
        calls[0]["args"],
        json!([
            "pane",
            "read",
            "pane-1-lead",
            "--source",
            "recent-unwrapped",
            "--lines",
            "2200",
            "--format",
            "ansi"
        ])
    );
    assert_eq!(calls[0]["socket"], "socket-a");
    assert!(calls[0]["pane"].is_null());
}

#[test]
fn history_from_a_replaced_native_session_is_never_sent() {
    let f = Fixture::new();
    let server = Server::start(&f);
    let mut responses: Value =
        serde_json::from_str(&fs::read_to_string(f.root.join("responses.json")).unwrap()).unwrap();
    let mut panes: Value =
        serde_json::from_str(responses["pane list --workspace w1"].as_str().unwrap()).unwrap();
    panes["result"]["panes"][0]["terminal_id"] = json!("replacement");
    responses["pane list --workspace w1"] = json!(panes.to_string());
    configure(
        &f,
        json!({"quiet":true,"history_mutate_metadata":responses}),
    );
    configure_history(&f, "PRIVATE-OLD-HISTORY\n");
    let mut live = server.live(&path("socket-a", "w1", "lead"));
    assert_eq!(live.record().unwrap()["kind"], "reset");
    let next = live.record().unwrap();
    assert_eq!(next["kind"], "closed");
    assert_eq!(next["reason"], "replaced");
    assert!(!next.to_string().contains("PRIVATE-OLD-HISTORY"));
    assert!(!history_invocations(&f).is_empty());
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
    for holding_write_lock in [false, true] {
        let f = Fixture::new();
        let server = Server::start(&f);
        let p = path("socket-a", "w1", "lead");
        configure(&f, json!({"quiet":true}));
        let mut live = server.live(&p);
        let reset = live.record().unwrap();
        let db = f.db();
        if holding_write_lock {
            db.execute_batch("BEGIN EXCLUSIVE").unwrap();
        }
        db.execute(
            "UPDATE pane_bindings SET binding_id = 'new-binding' WHERE address_id = ?1",
            [xws::address_id("w1", "lead")],
        )
        .unwrap();
        let event = live.record().unwrap();
        if holding_write_lock {
            assert_eq!(event["kind"], "unavailable");
            assert_eq!(event["reason"], "busy");
            db.execute_batch("COMMIT").unwrap();
        } else {
            // The writer can outlast the reader's bounded lock wait on a busy host.
            assert!(
                matches!(
                    (event["kind"].as_str(), event["reason"].as_str()),
                    (Some("closed"), Some("replaced")) | (Some("unavailable"), Some("busy"))
                ),
                "unexpected rebind event: {event}"
            );
        }
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
