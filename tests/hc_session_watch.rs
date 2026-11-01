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
    io::{BufRead, Write},
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Agent(Child);
impl Agent {
    fn new(home: &Path) -> Self {
        let child = Command::new("/bin/bash")
            .arg0("codex")
            .args([
                "--noprofile",
                "--norc",
                "-c",
                r#"trap 'printf "interrupted\n"; IFS= read -r release; [[ "$release" = release ]] || exit 1; exit 0' INT
printf 'ready\n'
while IFS= read -r line; do :; done
"#,
            ])
            .isolated_env(home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut agent = Self(child);
        let mut ready = String::new();
        std::io::BufReader::new(agent.0.stdout.as_mut().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready, "ready\n");
        agent
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
    t["status"] = json!({"state":"working"});
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
        json!({"alias":"alice","pane_type":"terminal"}),
        &runtime,
    )
    .unwrap();
    run(&fixture, &runtime, &mut agent);
    std::fs::remove_dir_all(state).unwrap();
}
#[test]
fn delayed_sigint_exit_clears_without_another_event() {
    for kind in ["foreground", "status", "status-null"] {
        check(kind, |fixture, runtime, agent| {
            let mut subscriber = namedrop::host::hc::events::Subscriber::default();
            subscriber
                .handle_line(
                    &json!({"terminals":*fixture.terminals.borrow()}).to_string(),
                    runtime,
                )
                .unwrap();
            if kind == "status-null" {
                fixture.terminals.borrow_mut()[0]["status"] = Value::Null;
            } else {
                fixture.terminals.borrow_mut()[0]["foreground"] =
                    json!({"pid":0,"name":"bash","argv":["bash"]});
            }
            assert_eq!(unsafe { libc::kill(agent.pid() as i32, libc::SIGINT) }, 0);
            let mut interrupted = String::new();
            std::io::BufReader::new(agent.0.stdout.as_mut().unwrap())
                .read_line(&mut interrupted)
                .unwrap();
            assert_eq!(interrupted, "interrupted\n");
            subscriber
                .handle_line(
                    &event(if kind == "status-null" {
                        "status"
                    } else {
                        kind
                    }),
                    runtime,
                )
                .unwrap();
            assert_eq!(
                fixture.terminals.borrow()[0]["labels"]["namedrop.session"],
                "original"
            );
            assert!(
                agent.0.try_wait().unwrap().is_none(),
                "owner must still be winding down at the event"
            );
            agent
                .0
                .stdin
                .as_mut()
                .unwrap()
                .write_all(b"release\n")
                .unwrap();
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
            let namespace = namedrop::StateNamespace::for_space("hc:fixture", "fixture-space");
            let addresses = namedrop::StateStore::open(runtime.state_directory)
                .unwrap()
                .transaction(|tx| tx.list_pane_addresses(&namespace))
                .unwrap();
            assert!(
                addresses
                    .iter()
                    .find(|address| address.alias == "alice")
                    .unwrap()
                    .agent_session_id
                    .is_none()
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

#[test]
fn background_watch_is_local_then_resumes_on_foreground_return() {
    check("return", |fixture, runtime, _| {
        let mut subscriber = namedrop::host::hc::events::Subscriber::default();
        subscriber
            .handle_line(
                &json!({"terminals": *fixture.terminals.borrow()}).to_string(),
                runtime,
            )
            .unwrap();
        let foreground = fixture.terminals.borrow()[0]["foreground"].clone();
        fixture.terminals.borrow_mut()[0]["foreground"] =
            json!({"pid":0,"name":"bash","argv":["bash"]});
        subscriber
            .handle_line(&event("foreground"), runtime)
            .unwrap();
        fixture.calls.borrow_mut().clear();
        for now in [10_500, 11_000, 11_500, 12_000, 12_500] {
            subscriber
                .tick(&LifecycleRuntime {
                    clock: &TestClock(now),
                    ..*runtime
                })
                .unwrap();
        }
        let gets = fixture
            .calls
            .borrow()
            .iter()
            .filter(|c| c[0] == "get")
            .count();
        assert_eq!(gets, 0, "live owners must not cause core reads");
        assert!(
            fixture
                .calls
                .borrow()
                .iter()
                .all(|c| c[0] != "ls" && !(c[0] == "get" && c[1] != "t1")),
            "{:?}",
            fixture.calls.borrow()
        );
        assert_eq!(
            fixture.terminals.borrow()[0]["labels"]["namedrop.session"],
            "original"
        );
        fixture.terminals.borrow_mut()[0]["foreground"] = foreground;
        subscriber
            .handle_line(
                &event("foreground"),
                &LifecycleRuntime {
                    clock: &TestClock(12_500),
                    ..*runtime
                },
            )
            .unwrap();
        fixture.calls.borrow_mut().clear();
        subscriber
            .tick(&LifecycleRuntime {
                clock: &TestClock(13_000),
                ..*runtime
            })
            .unwrap();
        assert!(
            fixture.calls.borrow().is_empty(),
            "foreground return left a watch armed"
        );
    });
}

#[test]
fn due_watch_preserves_and_disarms_for_replacement_generation() {
    check("replacement", |fixture, runtime, _| {
        fixture.terminals.borrow_mut()[0]["foreground"] =
            json!({"pid":0,"name":"bash","argv":["bash"]});
        let mut subscriber = namedrop::host::hc::events::Subscriber::default();
        subscriber
            .handle_line(
                &json!({"terminals": *fixture.terminals.borrow()}).to_string(),
                runtime,
            )
            .unwrap();
        fixture.terminals.borrow_mut()[0]["labels"]["namedrop.session"] = json!("replacement");
        subscriber
            .tick(&LifecycleRuntime {
                clock: &TestClock(10_500),
                ..*runtime
            })
            .unwrap();
        assert_eq!(
            fixture.terminals.borrow()[0]["labels"]["namedrop.session"],
            "replacement"
        );
        fixture.calls.borrow_mut().clear();
        subscriber
            .tick(&LifecycleRuntime {
                clock: &TestClock(11_000),
                ..*runtime
            })
            .unwrap();
        assert!(
            fixture.calls.borrow().is_empty(),
            "old generation watch survived replacement"
        );
    });
}

#[test]
fn matching_session_end_clears_live_owner_and_stale_end_preserves_replacement() {
    check("end", |fixture, runtime, agent| {
        runtime
            .host
            .end_session(runtime.state_directory, "t1", "stale")
            .unwrap();
        assert_eq!(
            fixture.terminals.borrow()[0]["labels"]["namedrop.session"],
            "original"
        );
        runtime
            .host
            .end_session(runtime.state_directory, "t1", "original")
            .unwrap();
        assert!(agent.0.try_wait().unwrap().is_none());
        for key in [
            "namedrop.session",
            "namedrop.session.pid",
            "namedrop.session.start",
        ] {
            assert_eq!(fixture.terminals.borrow()[0]["labels"][key], "");
        }
        fixture.terminals.borrow_mut()[0]["labels"]["namedrop.session"] = json!("replacement");
        runtime
            .host
            .end_session(runtime.state_directory, "t1", "original")
            .unwrap();
        assert_eq!(
            fixture.terminals.borrow()[0]["labels"]["namedrop.session"],
            "replacement"
        );
    });
}

#[test]
fn unactivated_workspace_has_no_owner_watch() {
    check("inactive", |fixture, runtime, _| {
        let fresh = runtime.state_directory.join("unactivated");
        let runtime = LifecycleRuntime {
            state_directory: &fresh,
            ..*runtime
        };
        fixture.terminals.borrow_mut()[0]["foreground"] =
            json!({"pid":0,"name":"bash","argv":["bash"]});
        let mut subscriber = namedrop::host::hc::events::Subscriber::default();
        subscriber
            .handle_line(
                &json!({"terminals": *fixture.terminals.borrow()}).to_string(),
                &runtime,
            )
            .unwrap();
        subscriber
            .handle_line(&event("foreground"), &runtime)
            .unwrap();
        fixture.calls.borrow_mut().clear();
        subscriber
            .tick(&LifecycleRuntime {
                clock: &TestClock(10_500),
                ..runtime
            })
            .unwrap();
        assert!(fixture.calls.borrow().is_empty());
        assert_eq!(
            fixture.terminals.borrow()[0]["labels"]["namedrop.session"],
            "original"
        );
    });
}

#[test]
fn live_owner_has_no_core_or_store_work_for_sixty_seconds() {
    for background in [false, true] {
        check(
            if background {
                "idle-background"
            } else {
                "idle-statusless"
            },
            |fixture, runtime, _| {
                fixture.terminals.borrow_mut()[0]["status"] = Value::Null;
                if background {
                    fixture.terminals.borrow_mut()[0]["foreground"] =
                        json!({"pid":0,"name":"bash","argv":["bash"]});
                }
                let mut subscriber = namedrop::host::hc::events::Subscriber::default();
                subscriber
                    .handle_line(
                        &json!({"terminals": *fixture.terminals.borrow()}).to_string(),
                        runtime,
                    )
                    .unwrap();
                let epoch = || {
                    rusqlite::Connection::open(runtime.state_directory.join("namedrop.sqlite3"))
                        .unwrap()
                        .query_row("SELECT epoch FROM reconciliation_epochs", [], |row| {
                            row.get::<_, i64>(0)
                        })
                        .unwrap()
                };
                let before = epoch();
                fixture.calls.borrow_mut().clear();
                for elapsed in (500..=60_000).step_by(500) {
                    subscriber
                        .tick(&LifecycleRuntime {
                            clock: &TestClock(10_000 + elapsed),
                            ..*runtime
                        })
                        .unwrap();
                }
                assert!(
                    fixture.calls.borrow().is_empty(),
                    "live owner caused {} core calls",
                    fixture.calls.borrow().len()
                );
                assert_eq!(epoch(), before, "live owner changed reconciliation epoch");
            },
        );
    }
}

#[test]
fn foreground_without_owner_watch_has_no_core_or_store_work() {
    check("unwatched-foreground", |fixture, runtime, _| {
        let mut subscriber = namedrop::host::hc::events::Subscriber::default();
        subscriber
            .handle_line(
                &json!({"terminals": *fixture.terminals.borrow()}).to_string(),
                runtime,
            )
            .unwrap();
        let epoch = || {
            rusqlite::Connection::open(runtime.state_directory.join("namedrop.sqlite3"))
                .unwrap()
                .query_row("SELECT epoch FROM reconciliation_epochs", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap()
        };
        let before = epoch();
        fixture.calls.borrow_mut().clear();
        subscriber
            .handle_line(
                &json!({"method":"event","params":{"type":"foreground","terminal":"t2"}})
                    .to_string(),
                runtime,
            )
            .unwrap();
        assert!(fixture.calls.borrow().is_empty());
        assert_eq!(epoch(), before);
    });
}
