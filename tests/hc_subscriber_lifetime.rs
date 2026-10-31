#[path = "support/environment.rs"]
mod environment;
#[path = "support/hc_socket.rs"]
mod hc_socket;

use environment::CommandHome;
use serde_json::Value;
use std::{
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Fixture {
    root: PathBuf,
    socket: Option<hc_socket::HcSocket>,
}

impl Fixture {
    fn new() -> Self {
        let root = hc_socket::fixture::short_socket_root();
        std::fs::create_dir(root.join("home")).unwrap();
        let socket = Some(hc_socket::HcSocket::bind(&root.join("core.sock")).unwrap());
        std::fs::write(
            root.join("hc"),
            format!(
                r#"#!/usr/bin/python3
import json, os, sys, time
root = os.environ['FIXTURE_ROOT']
with open(root+'/calls', 'a') as log:
    log.write(json.dumps(sys.argv[1:])+'\n')
if sys.argv[1:] == ['server', 'status', '--json']:
    print({status:?})
elif sys.argv[1:] == ['events', '--json']:
    if os.path.exists(root+'/eof'):
        raise SystemExit(0)
    print('{{"method":"heartbeat","params":{{}}}}', flush=True)
    while not os.path.exists(root+'/disconnect'):
        time.sleep(.01)
elif sys.argv[1:] == ['ls', '--json']:
    print('{{"terminals":[]}}')
else:
    raise SystemExit('unexpected command '+str(sys.argv))
"#,
                status = include_str!("fixtures/hc/server-status.stdout")
            ),
        )
        .unwrap();
        std::fs::set_permissions(root.join("hc"), std::fs::Permissions::from_mode(0o700)).unwrap();
        Self { root, socket }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_namedrop"));
        command
            .isolated_env(self.root.join("home"))
            .env("PATH", "/usr/bin:/bin")
            .env("HC_SOCKET", self.root.join("core.sock"))
            .env("HC_BIN_PATH", self.root.join("hc"))
            .env("FIXTURE_ROOT", &self.root)
            .args(["--host", "hc", "--plugin-state"])
            .arg(self.root.join("state"));
        command
    }

    fn spawn(&self) -> OwnedChild {
        OwnedChild(
            self.command()
                .arg("events")
                .stdout(Stdio::null())
                .stderr(std::fs::File::create(self.root.join("stderr")).unwrap())
                .spawn()
                .unwrap(),
        )
    }

    fn streams(&self) -> usize {
        std::fs::read_to_string(self.root.join("calls"))
            .unwrap_or_default()
            .lines()
            .filter(|line| *line == "[\"events\", \"--json\"]")
            .count()
    }

    fn directory(&self) -> PathBuf {
        std::fs::read_dir(self.root.join("state/hc-subscribers"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = namedrop::host::hc::events::stop(
            &self.root.join("state"),
            "hc:628bcf8129aa037b1fa5aad3504926a6",
        );
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
struct OwnedChild(Child);
impl OwnedChild {
    fn exited(&mut self) -> Option<std::process::ExitStatus> {
        self.0.try_wait().unwrap()
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn until(mut ready: impl FnMut() -> bool, seconds: u64, message: &str) {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while !ready() {
        assert!(Instant::now() < deadline, "{message}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn missing_and_refused_socket_stop_after_three_reconnects() {
    for missing in [true, false] {
        let mut fixture = Fixture::new();
        let mut child = fixture.spawn();
        until(|| fixture.streams() == 1, 5, "stream did not start");
        fixture.socket.take();
        if missing {
            std::fs::remove_file(fixture.root.join("core.sock")).unwrap();
        }
        std::fs::write(fixture.root.join("disconnect"), "").unwrap();
        until(
            || child.exited().is_some(),
            15,
            "subscriber remained alive after core loss",
        );
        assert!(child.exited().unwrap().success());
        let log = std::fs::read_to_string(fixture.root.join("stderr")).unwrap();
        assert_eq!(
            log.matches("Namedrop hc reconnect attempt").count(),
            3,
            "{log}"
        );
        let lock = std::fs::File::open(fixture.directory().join("lifetime.lock")).unwrap();
        lock.try_lock()
            .expect("exited subscriber retained lifetime lock");
    }
}

#[test]
fn empty_stream_reconnects_are_bounded() {
    let fixture = Fixture::new();
    std::fs::write(fixture.root.join("eof"), "").unwrap();
    let mut child = fixture.spawn();
    until(
        || child.exited().is_some(),
        15,
        "EOF kept subscriber alive indefinitely",
    );
    assert!(child.exited().unwrap().success());
    assert_eq!(
        fixture.streams(),
        4,
        "initial stream plus three reconnect attempts"
    );
}

#[test]
fn singleton_sigterm_and_dead_pid_recovery() {
    let fixture = Fixture::new();
    let mut first = fixture.spawn();
    until(|| fixture.streams() == 1, 5, "stream did not start");
    let mut duplicate = fixture.spawn();
    until(
        || duplicate.exited().is_some(),
        5,
        "duplicate subscriber did not exit",
    );
    assert!(duplicate.exited().unwrap().success());
    assert_eq!(fixture.streams(), 1);
    assert_eq!(unsafe { libc::kill(first.0.id() as i32, libc::SIGTERM) }, 0);
    until(|| first.exited().is_some(), 2, "SIGTERM was not responsive");
    assert!(first.exited().unwrap().success());
    let mut next = fixture.spawn();
    until(
        || fixture.streams() == 2,
        5,
        "stale record was not reclaimed",
    );
    next.0.kill().unwrap();
    next.0.wait().unwrap();
    let mut recovered = fixture.spawn();
    until(
        || fixture.streams() == 3,
        5,
        "dead PID lifetime lock was not reclaimed",
    );
    let record: Value = serde_json::from_slice(
        &std::fs::read(fixture.directory().join("subscriber.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(record["pid"], recovered.0.id());
    assert_eq!(
        unsafe { libc::kill(recovered.0.id() as i32, libc::SIGTERM) },
        0
    );
    until(
        || recovered.exited().is_some(),
        2,
        "recovered subscriber did not stop",
    );
}

#[test]
fn doctor_does_not_start_subscriber_without_active_worktree() {
    for existing in [false, true] {
        let fixture = Fixture::new();
        if existing {
            namedrop::StateStore::open(&fixture.root.join("state")).unwrap();
        }
        let output = fixture
            .command()
            .args(["--hc-space", "fixture", "doctor", "--json"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fixture.streams(),
            0,
            "doctor launched a subscriber without activation"
        );
        assert!(
            !fixture.root.join("state/hc-subscribers").exists(),
            "doctor created subscriber records without activation"
        );
    }
}
