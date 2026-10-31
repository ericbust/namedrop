#[path = "support/environment.rs"]
mod environment;
use environment::CommandHome;
#[allow(dead_code)]
#[path = "fixtures/hc/process.rs"]
mod process;
use namedrop::{Clock, HcHost, HerdrCommandExecutor, HerdrCommandOutput, LifecycleRuntime};
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    io::BufRead,
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Agent(Child);
impl Agent {
    fn new(home: &Path) -> Self {
        let mut child = Command::new("/usr/bin/python3").arg0("codex")
            .args(["-c", "import signal,time\ndef end(*args):\n time.sleep(0.15)\n raise SystemExit(0)\nsignal.signal(signal.SIGINT,end)\nprint('ready',flush=True)\nwhile True: time.sleep(1)"])
            .isolated_env(home).stdout(Stdio::piped()).spawn().unwrap();
        let mut ready = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready, "ready\n");
        Self(child)
    }
    fn pid(&self) -> u32 {
        self.0.id()
    }
}
impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
#[derive(Default)]
struct Fixture {
    terminals: RefCell<Vec<Value>>,
    calls: RefCell<Vec<Vec<String>>>,
}
impl HerdrCommandExecutor for Fixture {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        self.calls
            .borrow_mut()
            .push(args.iter().map(|s| s.to_string()).collect());
        let stdout = match args {
            ["ls", "--json"] => json!({"terminals":*self.terminals.borrow()}).to_string(),
            ["get", id, "--json"] => self
                .terminals
                .borrow()
                .iter()
                .find(|t| t["address"] == *id)
                .unwrap()
                .to_string(),
            ["label", id, labels @ ..] => {
                let mut all = self.terminals.borrow_mut();
                let t = all.iter_mut().find(|t| t["address"] == *id).unwrap();
                for label in labels.iter().filter(|s| **s != "--json") {
                    let (k, v) = label.split_once('=').unwrap();
                    t["labels"][k] = json!(v);
                }
                "{}".into()
            }
            ["server", "status", "--json"] => {
                include_str!("fixtures/hc/server-status.stdout").into()
            }
            ["--version"] => "herdr-core 0.1.14".into(),
            other => panic!("unexpected hc call {other:?}"),
        };
        Ok(HerdrCommandOutput { stdout })
    }
}
struct TestClock(u64);
impl Clock for TestClock {
    fn now_unix_ms(&self) -> u64 {
        self.0
    }
}
fn event(kind: &str) -> String {
    json!({"method":"event","params":{"type":kind,"terminal":"t1"}}).to_string()
}
fn check(
    name: &str,
    run: impl FnOnce(
        &Fixture,
        &LifecycleRuntime<'_, HcHost<&Fixture>, TestClock, namedrop::ProcessIds>,
        &mut Agent,
    ),
) {
    let state = std::env::temp_dir().join(format!(
        "namedrop-session-watch-{name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&state);
    let home = state.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let mut agent = Agent::new(&home);
    let fixture = Fixture::default();
    let mut t: Value =
        serde_json::from_str(include_str!("fixtures/hc/get-labeled.stdout")).unwrap();
    t["pid"] = json!(agent.pid());
    t["foreground"] = json!({"pid":agent.pid(),"name":"codex","argv":["codex"]});
    t["labels"]["namedrop.session"] = json!("original");
    t["labels"]["namedrop.session.pid"] = json!(agent.pid().to_string());
    t["labels"]["namedrop.session.start"] = json!(process::start(agent.pid()));
    let mut other = t.clone();
    other["address"] = json!("t2");
    other["labels"] = json!({"herdr.space":"other-space"});
    fixture.terminals.borrow_mut().extend([t, other]);
    let clock = TestClock(10_000);
    let ids = namedrop::ProcessIds::default();
    let runtime = LifecycleRuntime {
        state_directory: &state,
        home_directory: Some(&home),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new(env!("CARGO_MANIFEST_DIR")),
        workspace_id: Some("fixture-space"),
        current_pane_id: Some("t1"),
        session_socket_id: "hc:fixture",
        host: HcHost::new(&fixture),
        clock: &clock,
        ids: &ids,
    };
    namedrop::execute_lifecycle_operation(
        "name",
        json!({"alias":"alice","pane_type":"agent"}),
        &runtime,
    )
    .unwrap();
    run(&fixture, &runtime, &mut agent);
    std::fs::remove_dir_all(state).unwrap();
}
#[test]
fn delayed_sigint_exit_clears_without_another_event() {
    for kind in ["foreground", "status"] {
        check(kind, |fixture, runtime, agent| {
            let mut subscriber = namedrop::host::hc::events::Subscriber::default();
            subscriber
                .handle_line(
                    &json!({"terminals":*fixture.terminals.borrow()}).to_string(),
                    runtime,
                )
                .unwrap();
            fixture.terminals.borrow_mut()[0]["foreground"] =
                json!({"pid":0,"name":"bash","argv":["bash"]});
            assert_eq!(unsafe { libc::kill(agent.pid() as i32, libc::SIGINT) }, 0);
            subscriber.handle_line(&event(kind), runtime).unwrap();
            assert_eq!(
                fixture.terminals.borrow()[0]["labels"]["namedrop.session"],
                "original"
            );
            assert!(
                agent.0.try_wait().unwrap().is_none(),
                "owner must still be winding down at the event"
            );
            assert!(agent.0.wait().unwrap().success());
            fixture.calls.borrow_mut().clear();
            let started = Instant::now();
            for elapsed in [500, 1000, 1500, 2000] {
                subscriber
                    .tick(&LifecycleRuntime {
                        clock: &TestClock(10_000 + elapsed),
                        ..*runtime
                    })
                    .unwrap();
            }
            assert_eq!(
                fixture.terminals.borrow()[0]["labels"]["namedrop.session"],
                "",
                "session remained after due checks without another event"
            );
            assert!(started.elapsed() < Duration::from_secs(2));
            let calls = fixture.calls.borrow();
            assert!(
                calls
                    .iter()
                    .all(|c| c[0] != "ls" && !(c[0] == "get" && c[1] != "t1")),
                "untargeted calls {calls:?}"
            );
            assert!(
                calls.iter().filter(|c| c[0] == "get").count() <= 3,
                "unbounded checks {calls:?}"
            );
        });
    }
}
