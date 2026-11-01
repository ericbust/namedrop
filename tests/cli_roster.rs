#[path = "support/environment.rs"]
mod environment;

use environment::CommandHome;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
    path::Path,
    process::Command,
};

struct PrivateHome(std::path::PathBuf);

impl PrivateHome {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "namedrop-roster-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for PrivateHome {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn run(home: &Path, socket: &Path, host: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_namedrop"))
        .isolated_home(home)
        .env("HC_SOCKET", socket)
        .env("HC_BIN_PATH", "/bin/false")
        .env("NAMEDROP_STATE_DIR", home.join("state"))
        .args(["--host", host, "roster", "--json"])
        .output()
        .unwrap()
}

#[test]
fn absent_core_returns_empty_direct_snapshot_without_creating_state() {
    let home = PrivateHome::new();
    let socket = home.path().join("absent.sock");
    let output = run(home.path(), &socket, "hc");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({"core":null,"rows":[],"state_directory":home.path().join("state")})
    );
    assert!(!home.path().join("state").exists());
}

#[test]
fn stopped_core_returns_empty_snapshot() {
    let home = PrivateHome::new();
    let socket = home.path().join("stale.sock");
    drop(UnixListener::bind(&socket).unwrap());
    let output = run(home.path(), &socket, "hc");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(serde_json::from_slice::<Value>(&output.stdout).unwrap()["core"], Value::Null);
}

#[test]
fn running_core_uses_one_status_rpc_and_keeps_state_absent() {
    let home = PrivateHome::new();
    let socket = home.path().join("core.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let worker = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let request: Value = serde_json::from_str(&line).unwrap();
        let mut stream = reader.into_inner();
        writeln!(stream, "{}", json!({"id":request["id"],"result":{"core":{"id":"current"}}})).unwrap();
        request["method"].clone()
    });
    let output = run(home.path(), &socket, "hc");
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(worker.join().unwrap(), "server.status");
    assert_eq!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap(),
        json!({"core":"current","rows":[],"state_directory":home.path().join("state")})
    );
    assert!(!home.path().join("state").exists());
}

#[test]
fn tui_roster_uses_hidden_command_rejection() {
    let home = PrivateHome::new();
    let output = run(home.path(), &home.path().join("absent.sock"), "tui");
    assert_eq!(output.status.code(), Some(64), "{output:?}");
    let body: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["error"]["code"], "invalid_request");
    assert_eq!(body["error"]["message"], "roster requires --host hc");
}
