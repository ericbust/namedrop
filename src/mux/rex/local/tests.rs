use super::*;
use crate::mux::{
    Budget, MuxKind,
    rex::control::{Connection, tests::Server},
};
use serde_json::json;
use std::{
    io::{self as stdio},
    os::unix::net::UnixListener,
    path::PathBuf,
    process::{Child, Command},
    sync::atomic::AtomicBool,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const SERVE: &str = "NAMEDROP_REX_TEST_SERVE";

fn status(pid: u32, listener: Value) -> Value {
    json!({"pid": pid, "host_id": "host:1", "api_revision": 12, "listeners": [
        {"endpoint": "tcp://127.0.0.1:7000", "transport": "tcp", "primary": false},
        listener,
    ]})
}

fn unix_listener(path: &str) -> Value {
    json!({"endpoint": format!("unix://{path}"), "addresses": [path], "transport": "unix",
           "primary": true})
}

/// Answers one connection's hello and its `server.status` with `status`.
fn serve(stream: UnixStream, status: Value) {
    let mut server = Server::upgrade(stream);
    server.welcome();
    let request = server.message();
    server.send(json!({"type": "response", "id": request["id"], "seq": 0, "payload": status}));
    server.close_code();
}

/// The scripted server as its own process, so its pid is not the test's.
/// The parent runs it as a child with `NAMEDROP_REX_TEST_SERVE` naming the
/// socket to listen on.
#[test]
#[ignore = "a child process of the prove_local tests"]
fn serve_status_as_a_child_process() {
    let Some(path) = std::env::var_os(SERVE) else {
        return;
    };
    let listener = UnixListener::bind(&path).unwrap();
    let (stream, _) = listener.accept().unwrap();
    let path = path.to_str().unwrap().to_owned();
    serve(stream, status(std::process::id(), unix_listener(&path)));
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let directory =
            std::env::temp_dir().join(format!("nd-local-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        Self(directory)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn child_server(socket: &Path) -> Child {
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "mux::rex::local::tests::serve_status_as_a_child_process",
            "--exact",
            "--ignored",
            "--test-threads=1",
        ])
        .env(SERVE, socket)
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !socket.exists() {
        assert!(Instant::now() < deadline, "the child server never listened");
        thread::sleep(Duration::from_millis(10));
    }
    child
}

/// Accepts one connection on `listen` and copies its bytes to and from `target`.
pub(in crate::mux::rex) fn forward(listen: &Path, target: PathBuf) -> JoinHandle<()> {
    let listener = UnixListener::bind(listen).unwrap();
    thread::spawn(move || {
        let (mut client, _) = listener.accept().unwrap();
        let mut server = UnixStream::connect(target).unwrap();
        let (mut client_back, mut server_back) =
            (client.try_clone().unwrap(), server.try_clone().unwrap());
        let up = thread::spawn(move || {
            let _ = stdio::copy(&mut client, &mut server);
            let _ = server.shutdown(std::net::Shutdown::Write);
        });
        let _ = stdio::copy(&mut server_back, &mut client_back);
        let _ = client_back.shutdown(std::net::Shutdown::Write);
        up.join().unwrap();
    })
}

fn prove(dial: &Path, stream: UnixStream) -> Result<LocalServer, Fault> {
    let peer = peer_pid(&stream).unwrap();
    let budget = || Ok(Budget::ADAPTER_DEFAULT);
    let cancelled = AtomicBool::new(false);
    let mut control = Connection::open(
        stream,
        super::super::control::Limits {
            budget: Box::new(budget),
            cancelled: &cancelled,
        },
    )
    .unwrap();
    prove_local(dial, &mut control, peer)
}

#[test]
fn the_peer_pid_of_a_socket_pair_is_this_process() {
    let (one, _other) = UnixStream::pair().unwrap();
    assert_eq!(peer_pid(&one).unwrap(), std::process::id());
}

#[test]
fn a_server_whose_pid_is_the_peer_is_local_and_named_by_its_primary_unix_listener() {
    let scratch = Scratch::new("direct");
    let socket = scratch.0.join("s.sock");
    let mut child = child_server(&socket);
    let stream = UnixStream::connect(&socket).unwrap();
    assert_eq!(peer_pid(&stream).unwrap(), child.id());
    let server = prove(&socket, stream).unwrap();
    assert_eq!(server.endpoint.kind, MuxKind::Rex);
    assert_eq!(server.endpoint.key, format!("rex:{}", socket.display()));
    assert_eq!(server.endpoint.unix_socket(), Some(socket.as_path()));
    assert_eq!(server.pid, child.id());
    assert_eq!(server.host_id.as_deref(), Some("host:1"));
    assert!(child.wait().unwrap().success());
}

#[test]
fn a_forwarded_socket_to_a_local_server_is_refused() {
    let scratch = Scratch::new("forwarded");
    let (socket, forwarded) = (scratch.0.join("s.sock"), scratch.0.join("f.sock"));
    let mut child = child_server(&socket);
    let forwarder = forward(&forwarded, socket.clone());
    let refused = prove(&forwarded, UnixStream::connect(&forwarded).unwrap()).map(|_| ());
    assert_eq!(
        refused,
        Err(Fault::NotLocal(format!(
            "{} is not a Rex server on this machine: the server is process {}, and the \
             connection's peer is process {}",
            forwarded.display(),
            child.id(),
            std::process::id()
        )))
    );
    forwarder.join().unwrap();
    assert!(child.wait().unwrap().success());
}

#[test]
fn a_server_whose_primary_listener_is_not_a_unix_socket_is_refused() {
    let own = std::process::id();
    for (listener, shown) in [
        (
            json!({"endpoint": "tcp://127.0.0.1:7000", "transport": "tcp", "primary": true}),
            r#"{"endpoint":"tcp://127.0.0.1:7000","primary":true,"transport":"tcp"}"#,
        ),
        (
            json!({"transport": "pipe", "addresses": ["/tmp/s.sock"], "primary": true}),
            r#"{"addresses":["/tmp/s.sock"],"primary":true,"transport":"pipe"}"#,
        ),
        (
            json!({"transport": "unix", "addresses": ["s.sock"], "primary": true}),
            r#"{"addresses":["s.sock"],"primary":true,"transport":"unix"}"#,
        ),
        (
            json!({"transport": "unix", "addresses": ["/tmp/s.sock"], "primary": false}),
            "null",
        ),
    ] {
        let (client, server) = UnixStream::pair().unwrap();
        let script = thread::spawn(move || serve(server, status(own, listener)));
        assert_eq!(
            prove(Path::new("/tmp/d.sock"), client).map(|_| ()),
            Err(Fault::NotLocal(format!(
                "/tmp/d.sock is not a Rex server on this machine: its primary listener is not a \
                 Unix socket: {shown}"
            )))
        );
        script.join().unwrap();
    }
}
