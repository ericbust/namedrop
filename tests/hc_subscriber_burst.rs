#[path = "support/environment.rs"]
mod environment;
#[allow(dead_code)]
#[path = "fixtures/hc/process.rs"]
mod process;
#[allow(dead_code)]
#[path = "support/socket_fixture.rs"]
mod socket_fixture;

use environment::CommandHome;
use namedrop::*;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::Path,
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Default)]
struct World {
    terminals: Vec<Value>,
    requests: Vec<Value>,
    events: Vec<Value>,
}
struct Setup(Arc<Mutex<World>>);
impl HerdrCommandExecutor for Setup {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let mut world = self.0.lock().unwrap();
        let value = match args {
            ["ls", "--json"] => json!({"terminals":world.terminals}),
            ["get", pane, "--json"] => world
                .terminals
                .iter()
                .find(|t| t["address"] == *pane)
                .unwrap()
                .clone(),
            ["server", "status", "--json"] => json!({"core":{"id":"burst-private"}}),
            ["--version"] => {
                return Ok(HerdrCommandOutput {
                    stdout: "herdr-core 0.1.14".into(),
                });
            }
            ["label", pane, labels @ ..] => {
                let terminal = world
                    .terminals
                    .iter_mut()
                    .find(|t| t["address"] == *pane)
                    .unwrap();
                for label in labels.iter().filter(|label| **label != "--json") {
                    let (key, value) = label.split_once('=').unwrap();
                    terminal["labels"][key] = json!(value);
                }
                json!({})
            }
            ["send", ..] => json!({"sent":true}),
            ["read", ..] => json!({"text":""}),
            other => panic!("unexpected setup command {other:?}"),
        };
        Ok(HerdrCommandOutput {
            stdout: value.to_string(),
        })
    }
}

struct Core {
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Core {
    fn start(path: &Path, world: Arc<Mutex<World>>) -> Self {
        socket_fixture::assert_socket_path(path);
        let listener = UnixListener::bind(path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let worker = thread::spawn(move || {
            let mut clients = Vec::new();
            while !stopped.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let world = world.clone();
                        let stopped = stopped.clone();
                        clients.push(thread::spawn(move || serve(stream, world, stopped)));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1))
                    }
                    Err(error) => panic!("private core accept: {error}"),
                }
            }
            for client in clients {
                client.join().unwrap();
            }
        });
        Self {
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Core {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}
fn serve(mut stream: UnixStream, world: Arc<Mutex<World>>, stop: Arc<AtomicBool>) {
    stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    while !stop.load(Ordering::Relaxed) {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => return,
            Ok(_) => (),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(_) => return,
        }
        let request: Value = serde_json::from_str(&line).unwrap();
        let method = request["method"].as_str().unwrap();
        let mut state = world.lock().unwrap();
        state.requests.push(request.clone());
        let result = match method {
            "server.status" => json!({"core":{"id":"burst-private"}}),
            "events.subscribe" => json!({"channel":null,"seq":0,"terminals":state.terminals}),
            "terminal.list" => json!({"terminals":state.terminals}),
            "terminal.get" => {
                match state
                    .terminals
                    .iter()
                    .find(|t| t["address"] == request["params"]["terminal"])
                {
                    Some(terminal) => terminal.clone(),
                    None => {
                        drop(state);
                        if writeln!(stream, "{}", json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32000,"message":"terminal not found","data":{"kind":"not_found","hint":"see hc ls --all"}}})).is_err() { return; }
                        continue;
                    }
                }
            }
            "terminal.label" => {
                let terminal = state
                    .terminals
                    .iter_mut()
                    .find(|t| t["address"] == request["params"]["terminal"])
                    .unwrap();
                for (key, value) in request["params"]["labels"].as_object().unwrap() {
                    terminal["labels"][key] = value.clone();
                }
                json!({})
            }
            other => panic!("unexpected subscriber request {other}: {request}"),
        };
        drop(state);
        if writeln!(
            stream,
            "{}",
            json!({"jsonrpc":"2.0","id":request["id"],"result":result})
        )
        .is_err()
        {
            return;
        }
        if method == "events.subscribe" {
            let mut cursor = 0;
            while !stop.load(Ordering::Relaxed) {
                let events = world.lock().unwrap().events.clone();
                let wave = events[cursor..]
                    .iter()
                    .map(|event| format!("{event}\n"))
                    .collect::<String>();
                if stream.write_all(wave.as_bytes()).is_err() {
                    return;
                }
                cursor = events.len();
                thread::sleep(Duration::from_millis(1));
            }
            return;
        }
    }
}
struct Subscriber(Child);
struct FixtureRoot(std::path::PathBuf);
impl Drop for FixtureRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
impl Drop for Subscriber {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn terminal(pane: &str, pid: u32, agent: Option<&str>) -> Value {
    let mut t: Value =
        serde_json::from_str(include_str!("fixtures/hc/get-labeled.stdout")).unwrap();
    t["address"] = json!(pane);
    t["pid"] = json!(pid);
    t["labels"] = json!({"herdr.space":"fixture-space"});
    t["foreground"] = json!({"pid":pid,"name":"bash","argv":["bash"]});
    if let Some(alias) = agent {
        t["foreground"] = json!({"pid":pid,"name":"claude","argv":["claude"]});
        t["status"] = json!({"app":"claude-code","state":"done"});
        t["status_ms"] = json!(1);
        t["labels"]["namedrop.name"] = json!(alias);
        t["labels"]["namedrop.session"] = json!(format!("session-{alias}"));
        t["labels"]["namedrop.session.pid"] = json!(pid.to_string());
        t["labels"]["namedrop.session.start"] = json!(process::start(pid));
    }
    t
}
fn quiet(world: &Arc<Mutex<World>>, subscriber: &mut Subscriber) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut previous = 0;
    let mut unchanged = Instant::now();
    loop {
        assert!(
            subscriber.0.try_wait().unwrap().is_none(),
            "subscriber exited"
        );
        let count = world.lock().unwrap().requests.len();
        if count != previous {
            previous = count;
            unchanged = Instant::now();
        }
        if count > 0 && unchanged.elapsed() >= Duration::from_millis(800) {
            return;
        }
        assert!(Instant::now() < deadline, "subscriber never became quiet");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn registered_agents_do_not_rescan_during_split_shell_burst() {
    exercise_burst(true);
}

#[test]
fn registered_agents_and_settled_mailbox_are_quiet_without_burst() {
    exercise_burst(false);
}

fn exercise_burst(burst: bool) {
    let root = socket_fixture::short_socket_root();
    let _root_guard = FixtureRoot(root.clone());
    let state = root.join("state");
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let agents: Vec<_> = (0..3)
        .map(|_| process::AgentProcess::with_program("claude"))
        .collect();
    let shell = process::AgentProcess::shell();
    let world = Arc::new(Mutex::new(World::default()));
    for (index, alias) in ["alpha", "bravo", "charlie"].iter().enumerate() {
        world.lock().unwrap().terminals.push(terminal(
            &format!("t{}", index + 1),
            agents[index].pid(),
            Some(alias),
        ));
    }
    let setup = Setup(world.clone());
    let ids = ProcessIds::default();
    let clock = SystemClock;
    let namespace = StateNamespace::for_space("hc:burst-private", "fixture-space");
    for (index, alias) in ["alpha", "bravo", "charlie"].iter().enumerate() {
        let pane = format!("t{}", index + 1);
        let runtime = LifecycleRuntime {
            state_directory: &state,
            home_directory: Some(&home),
            agent_config: AgentConfigDirectories::default(),
            cwd: Path::new(env!("CARGO_MANIFEST_DIR")),
            workspace_id: Some("fixture-space"),
            current_pane_id: Some(&pane),
            session_socket_id: "hc:burst-private",
            host: HcHost::new(&setup),
            clock: &clock,
            ids: &ids,
        };
        execute_lifecycle_operation("name", json!({"alias":alias,"pane_type":"agent"}), &runtime)
            .unwrap();
    }
    let attribution = MutationAttribution::new(ActorKind::Human, "burst-mail", clock.now_unix_ms());
    StateStore::open(&state)
        .unwrap()
        .transaction(|tx| {
            for binding in tx.active_pane_bindings(&namespace)? {
                let pane = &binding.pane_id;
                let address = tx
                    .list_pane_addresses(&namespace)?
                    .into_iter()
                    .find(|a| a.id == binding.address_id)
                    .unwrap();
                let session = address.agent_session_id.unwrap();
                let thread = ThreadId::new(format!("thread-{pane}"));
                tx.create_thread(&namespace, &thread, "settled burst control", &attribution)?;
                tx.append_message(
                    &namespace,
                    &MessageDraft {
                        thread_id: thread,
                        message_id: MessageId::new(format!("message-{pane}")),
                        in_reply_to: None,
                        author_address_id: address.id.clone(),
                        author_kind: ActorKind::Human,
                        author_alias_snapshot: address.alias.clone(),
                        author_agent_session_snapshot: None,
                        request_id: format!("request-{pane}"),
                        authored_source: "settled control".into(),
                        body: "settled control".into(),
                        grammar_version: "mention/v1".into(),
                        recipients: vec![MessageRecipientDraft {
                            address_id: address.id.clone(),
                            alias_snapshot: address.alias,
                            agent_session_snapshot: Some(session.clone()),
                            delivery_id: DeliveryId::new(format!("delivery-{pane}")),
                        }],
                        context_references: vec![],
                    },
                    &attribution,
                )?;
                let attempt = DeliveryAttemptId::new(format!("attempt-{pane}"));
                tx.lease_mailbox_head(
                    &namespace,
                    &address.id,
                    &LeaseDraft {
                        attempt_id: attempt.clone(),
                        binding_id: binding.id,
                        agent_session_id: session.clone(),
                        retry_of_attempt_id: None,
                        owner: "burst-setup".into(),
                        acquired_at_ms: clock.now_unix_ms(),
                        expires_at_ms: clock.now_unix_ms() + 60_000,
                    },
                    &attribution,
                )?;
                tx.transition_delivery_attempt(
                    &namespace,
                    &attempt,
                    DeliveryState::Submitted,
                    &attribution,
                )?;
                tx.acknowledge_delivery_attempt(
                    &namespace,
                    &attempt,
                    &format!("ack-{pane}"),
                    &session,
                    &attribution,
                )?;
                tx.transition_delivery_attempt(
                    &namespace,
                    &attempt,
                    DeliveryState::Settled,
                    &attribution,
                )?;
            }
            Ok(())
        })
        .unwrap();
    let snapshot = || {
        StateStore::open(&state)
            .unwrap()
            .transaction(|tx| {
                let addresses = tx.list_pane_addresses(&namespace)?;
                assert_eq!(addresses.len(), 3);
                for address in &addresses {
                    assert!(address.pane_id.is_some());
                    assert_eq!(
                        tx.delivery_attempt_state(
                            &namespace,
                            &DeliveryAttemptId::new(format!(
                                "attempt-{}",
                                address.pane_id.as_ref().unwrap()
                            ))
                        )?,
                        DeliveryState::Settled
                    );
                    assert!(
                        tx.active_integration_registration(
                            &namespace,
                            &address.id,
                            address.agent_session_id.as_ref().unwrap()
                        )?
                        .is_some()
                    );
                }
                Ok(serde_json::to_value(addresses).unwrap())
            })
            .unwrap()
    };
    let before = snapshot();
    let socket = root.join("core.sock");
    let core = Core::start(&socket, world.clone());
    let tripwire = root.join("hc-forbidden");
    std::fs::write(
        &tripwire,
        format!(
            "#!/bin/sh\necho invoked >> '{}'\nexit 99\n",
            root.join("cli-calls").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&tripwire, std::fs::Permissions::from_mode(0o700)).unwrap();
    let stderr = std::fs::File::create(root.join("subscriber.stderr")).unwrap();
    let mut subscriber = Subscriber(
        Command::new(env!("CARGO_BIN_EXE_namedrop"))
            .args(["--host", "hc", "--plugin-state"])
            .arg(&state)
            .arg("events")
            .isolated_env(&home)
            .env("PATH", "/usr/bin:/bin")
            .env("HC_SOCKET", &socket)
            .env("HC_SESSION", "burst-private")
            .env("HC_BIN_PATH", &tripwire)
            .env("CODEX_HOME", root.join("codex"))
            .env("CLAUDE_CONFIG_DIR", root.join("claude"))
            .stdout(Stdio::null())
            .stderr(stderr)
            .spawn()
            .unwrap(),
    );
    quiet(&world, &mut subscriber);
    assert_eq!(snapshot(), before);
    assert!(
        world
            .lock()
            .unwrap()
            .requests
            .iter()
            .any(|r| r["method"] == "events.subscribe")
    );
    if !burst {
        let count = world.lock().unwrap().requests.len();
        thread::sleep(Duration::from_millis(800));
        assert_eq!(
            world.lock().unwrap().requests.len(),
            count,
            "idle subscriber kept making requests"
        );
        assert_eq!(snapshot(), before);
        assert!(!root.join("cli-calls").exists());
        eprintln!(
            "control: three active claude registrations, three settled deliveries, aliases preserved, quiet wire, zero CLI fallback"
        );
        drop(subscriber);
        drop(core);
        return;
    }
    world.lock().unwrap().requests.clear();
    let started = Instant::now();
    for index in 0..11 {
        let target = Duration::from_micros(index * 17_800);
        if let Some(delay) = target.checked_sub(started.elapsed()) {
            thread::sleep(delay);
        }
        let pane = if index == 5 {
            "gone".to_owned()
        } else {
            format!("sh{index}")
        };
        let mut w = world.lock().unwrap();
        w.terminals.push(terminal(&pane, shell.pid(), None));
        let seq = w.events.len() + 1;
        w.events.push(json!({"jsonrpc":"2.0","method":"event","params":{"seq":seq,"type":"created","terminal":pane}}));
        if index == 5 {
            w.terminals.retain(|t| t["address"] != pane);
            for kind in ["lost", "removed"] {
                let seq = w.events.len() + 1;
                w.events.push(json!({"jsonrpc":"2.0","method":"event","params":{"seq":seq,"type":kind,"terminal":pane}}));
            }
        }
    }
    eprintln!("11 creation waves emitted over {:?}", started.elapsed());
    quiet(&world, &mut subscriber);
    assert_eq!(snapshot(), before, "burst changed registered addresses");
    assert!(
        !root.join("cli-calls").exists(),
        "subscriber used CLI fallback"
    );
    let requests = world.lock().unwrap().requests.clone();
    let mut counts = BTreeMap::<String, usize>::new();
    for request in &requests {
        let key = format!(
            "{} {}",
            request["method"].as_str().unwrap(),
            request["params"]["terminal"].as_str().unwrap_or("")
        );
        *counts.entry(key).or_default() += 1;
    }
    eprintln!("burst wire counts {counts:?}; fixture {}", root.display());
    for index in (0..11).filter(|i| *i != 5) {
        assert!(
            counts
                .get(&format!("terminal.get sh{index}"))
                .copied()
                .unwrap_or(0)
                >= 1,
            "survivor sh{index} skipped: {counts:?}"
        );
    }
    let lists = requests
        .iter()
        .filter(|r| r["method"] == "terminal.list")
        .count();
    let gets = requests
        .iter()
        .filter(|r| r["method"] == "terminal.get")
        .count();
    assert!(
        lists + gets <= 33,
        "11 created terminals permit at most 33 discovery RPCs; terminal.list={lists}, terminal.get={gets}; {counts:?}"
    );
    assert!(
        requests.len() <= 33,
        "total wire RPC budget exceeded: {counts:?}"
    );
    assert_eq!(lists, 0, "ordinary events must not list all terminals");
    for pane in ["t1", "t2", "t3"] {
        assert_eq!(
            counts
                .get(&format!("terminal.get {pane}"))
                .copied()
                .unwrap_or(0),
            0,
            "cached agent rescanned"
        );
    }
    assert!(
        counts.get("terminal.get gone").copied().unwrap_or(0) <= 1,
        "lost+removed reread the vanished pane"
    );
    drop(subscriber);
    drop(core);
}
