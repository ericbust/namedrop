use super::*;
use crate::{
    SpaceId,
    mux::{
        Endpoint, MuxKind, PaneId,
        home::{Claimed, HookHome, Inherited, Location, context_hook_on},
        processes::ThisMachine,
        rex::tests::{Local, State},
    },
};
use serde_json::Value;
use std::{collections::HashMap, ffi::OsString};

const BLOCK: &str = "block:0f6c0d4e-1b5a-4f8e-9d2c-3a7b8c9d0e1f";
const ENDPOINT: &str = "rex:/tmp/r.sock";

fn cases() -> Vec<Value> {
    let fixture: Value =
        serde_json::from_str(include_str!("../fixtures/hooks/chains.json")).unwrap();
    fixture["cases"].as_array().unwrap().clone()
}

fn case(name: &str) -> Value {
    cases()
        .into_iter()
        .find(|case| case["name"].as_str().unwrap().starts_with(name))
        .unwrap_or_else(|| panic!("no hook chain {name}"))
}

/// A hook as a fixture measured it: the caller's place, and this machine's
/// processes, where every listed process is alive and starts at `<pid>000`.
struct Hook {
    place: Place,
    local: Local,
    runtime: AgentRuntime,
    entrypoint: Option<String>,
}

impl Hook {
    fn of(case: &Value) -> Self {
        let local = Local::default();
        let ancestors: Vec<u32> = case["chain"]
            .as_array()
            .unwrap()
            .iter()
            .map(|link| {
                let pid = u32::try_from(link["pid"].as_u64().unwrap()).unwrap();
                let group = u32::try_from(link["group"].as_u64().unwrap()).unwrap();
                local.alive(pid, u64::from(pid) * 1000, group);
                local.runs(pid, link["executable"].as_str().unwrap());
                pid
            })
            .collect();
        let foreground: Proc = serde_json::from_value(case["foreground"].clone()).unwrap();
        if !ancestors.contains(&foreground.pid) {
            local.alive(
                foreground.pid,
                u64::from(foreground.pid) * 1000,
                foreground.pid,
            );
        }
        Self {
            place: Place {
                endpoint: Endpoint::parse(ENDPOINT),
                space: Some(SpaceId::new("session:s")),
                pane: Some(PaneId::new(BLOCK)),
                caller: Some(Box::new(Caller {
                    foreground: Some(foreground),
                    record: serde_json::from_value(case["record"].clone()).unwrap(),
                    ancestors,
                })),
            },
            local,
            runtime: AgentRuntime::parse(case["runtime"].as_str().unwrap()).unwrap(),
            entrypoint: case["entrypoint"].as_str().map(str::to_owned),
        }
    }

    fn entrypoint(mut self, entrypoint: Option<&str>) -> Self {
        self.entrypoint = entrypoint.map(str::to_owned);
        self
    }

    fn runtime(mut self, runtime: AgentRuntime) -> Self {
        self.runtime = runtime;
        self
    }

    fn record(mut self, record: Option<StatusRecord>) -> Self {
        self.caller().record = record;
        self
    }

    fn caller(&mut self) -> &mut Caller {
        self.place.caller.as_mut().unwrap()
    }

    fn foreground(&self) -> u32 {
        self.place
            .caller
            .as_ref()
            .unwrap()
            .foreground
            .as_ref()
            .unwrap()
            .pid
    }

    fn bind(&self, state: &State, session: &str, now_ms: u64) -> Result<(), BindRefused> {
        self.place.bind(
            &state.0,
            &AgentSessionId::new(session),
            self.runtime,
            self.entrypoint.as_deref().map(OsStr::new),
            &self.local,
            now_ms,
        )
    }
}

fn incarnation(pid: u32) -> String {
    format!("{pid}@{pid}000")
}

/// A binding of `BLOCK` whose foreground and agent are both `agent`.
fn bound_to(agent: u32, session: &str) -> Binding {
    Binding {
        block: BLOCK.into(),
        endpoint: ENDPOINT.into(),
        space: "session:s".into(),
        foreground: incarnation(agent),
        agent: incarnation(agent),
        runtime: "claude-code".into(),
        session: session.into(),
        bound_at_ms: 1,
    }
}

fn binding(state: &State) -> Option<Binding> {
    Bindings::new(&state.0).read(BLOCK)
}

fn refusal_of(state: &State) -> Option<Refusal> {
    Bindings::new(&state.0).refusal(BLOCK)
}

#[test]
fn each_measured_hook_chain_binds_or_refuses_as_the_design_measured() {
    for case in cases() {
        let name = case["name"].as_str().unwrap();
        let state = State::new();
        let hook = Hook::of(&case);
        if let Some(agent) = case["bound_agent"].as_u64() {
            state.bind(&bound_to(u32::try_from(agent).unwrap(), "outer"));
        }
        let bound = hook.bind(&state, "session-new", 1000);
        match case["expect"].as_str().unwrap() {
            "bind" => {
                assert_eq!(bound, Ok(()), "{name}");
                let agent = u32::try_from(case["agent"].as_u64().unwrap()).unwrap();
                assert_eq!(
                    binding(&state),
                    Some(Binding {
                        foreground: incarnation(hook.foreground()),
                        agent: incarnation(agent),
                        runtime: hook.runtime.as_str().into(),
                        session: "session-new".into(),
                        bound_at_ms: 1000,
                        ..bound_to(agent, "")
                    }),
                    "{name}"
                );
            }
            refused => {
                assert_eq!(
                    bound
                        .as_ref()
                        .map_err(|why| serde_json::to_value(why).unwrap()),
                    Err(Value::from(refused)),
                    "{name}"
                );
                assert_eq!(
                    binding(&state).map(|binding| binding.session),
                    case["bound_agent"].as_u64().map(|_| "outer".to_owned()),
                    "{name}: a refused hook leaves the block's binding as it was"
                );
            }
        }
    }
}

#[test]
fn a_hook_whose_runtime_is_not_the_foregrounds_record_refuses() {
    let state = State::new();
    let hook = Hook::of(&case("Claude Code 2.1.323")).runtime(AgentRuntime::Codex);
    assert_eq!(
        hook.bind(&state, "session-1", 1000),
        Err(BindRefused::Runtime)
    );
    let codex = Hook::of(&case("Codex 0.163 with --no-daemon")).runtime(AgentRuntime::ClaudeCode);
    assert_eq!(
        codex.bind(&state, "session-1", 1000),
        Err(BindRefused::Runtime)
    );
}

/// The record is checked only once it speaks for the foreground: a hook can
/// beat its agent's first report, and after a crash the dead agent's record
/// can still stand when the new agent's SessionStart runs.
#[test]
fn a_session_start_with_no_record_or_a_dead_agents_record_binds() {
    let claude = || Hook::of(&case("Claude Code 2.1.323"));
    let stale = StatusRecord {
        owner: Some(super::super::wire::Owner {
            pid: 11111,
            name: "codex".into(),
        }),
        app: Some("codex".into()),
        ..claude().place.caller.unwrap().record.unwrap()
    };
    for hook in [claude().record(None), claude().record(Some(stale))] {
        let state = State::new();
        assert_eq!(hook.bind(&state, "session-1", 1000), Ok(()));
        assert_eq!(binding(&state).unwrap().session, "session-1");
    }
}

#[test]
fn a_hook_whose_agent_is_the_foreground_replaces_a_binding_that_names_another_agent() {
    let state = State::new();
    state.bind(&bound_to(4242, "older"));
    let hook = Hook::of(&case("Claude Code 2.1.323"));
    assert_eq!(hook.bind(&state, "session-1", 1000), Ok(()));
    let bound = binding(&state).unwrap();
    assert_eq!(
        (bound.agent, bound.session),
        (incarnation(26048), "session-1".into())
    );
}

/// Condition 5: behind the foreground, a hook cannot take a block whose
/// binding names the foreground itself as its agent.
#[test]
fn a_hook_behind_the_foreground_refuses_when_the_foreground_is_the_bound_agent() {
    let state = State::new();
    let hook = Hook::of(&case("Claude Code behind bash wrap.sh, SessionStart"));
    state.bind(&bound_to(82428, "the launcher"));
    assert_eq!(
        hook.bind(&state, "session-1", 1000),
        Err(BindRefused::AlreadyBound)
    );
    state.bind(&Binding {
        agent: incarnation(82429),
        ..bound_to(82428, "the agent")
    });
    assert_eq!(hook.bind(&state, "session-2", 1000), Ok(()));
    assert_eq!(binding(&state).unwrap().session, "session-2");
}

/// Condition 6 refuses only `sdk-cli` behind the foreground. A Claude Code
/// that is the foreground binds whatever entrypoint its block inherited.
#[test]
fn only_a_print_mode_claude_code_behind_the_foreground_is_refused_by_its_entrypoint() {
    let behind = || Hook::of(&case("Claude Code behind bash wrap.sh, SessionStart"));
    let state = State::new();
    assert_eq!(
        behind()
            .entrypoint(Some("sdk-cli"))
            .bind(&state, "session-1", 1000),
        Err(BindRefused::NestedPrintMode)
    );
    for entrypoint in [Some("cli"), None] {
        let state = State::new();
        assert_eq!(
            behind()
                .entrypoint(entrypoint)
                .bind(&state, "session-1", 1000),
            Ok(()),
            "{entrypoint:?}"
        );
    }
    for entrypoint in [
        Some("cli"),
        Some("sdk-cli"),
        Some("sdk-ts"),
        Some("sdk-py"),
        Some("claude-vscode"),
        Some("local-agent"),
        None,
    ] {
        let state = State::new();
        let hook = Hook::of(&case("Claude Code 2.1.323")).entrypoint(entrypoint);
        assert_eq!(
            hook.bind(&state, "session-1", 1000),
            Ok(()),
            "{entrypoint:?}"
        );
    }
}

#[test]
fn a_refusal_is_recorded_for_doctor_and_a_later_bind_deletes_it() {
    let state = State::new();
    let nested = Hook::of(&case("claude -p run as a tool"));
    assert_eq!(
        nested.bind(&state, "nested-session", 1000),
        Err(BindRefused::NotInGroup)
    );
    assert_eq!(
        refusal_of(&state),
        Some(Refusal {
            block: BLOCK.into(),
            endpoint: ENDPOINT.into(),
            condition: BindRefused::NotInGroup,
            session: "nested-session".into(),
            foreground: Some(incarnation(26048)),
            at_ms: 1000,
        })
    );
    let launched = Hook::of(&case("claude -p started by a Python MCP server"));
    assert_eq!(
        launched.bind(&state, "mcp-session", 2000),
        Err(BindRefused::Launcher)
    );
    assert_eq!(
        refusal_of(&state).map(|refusal| (refusal.condition, refusal.session)),
        Some((BindRefused::Launcher, "mcp-session".into())),
        "the latest refusal replaces the one before"
    );
    let outer = Hook::of(&case("Claude Code 2.1.323"));
    assert_eq!(outer.bind(&state, "outer-session", 3000), Ok(()));
    assert_eq!(refusal_of(&state), None);
}

/// A nested hook refused in a block whose binding names the block's
/// foreground leaves no refusal: the block is served. Under a binding to
/// another foreground the refusal is news, and it names its foreground.
#[test]
fn a_refusal_is_recorded_only_while_the_binding_names_another_foreground() {
    let nested = || Hook::of(&case("claude -p run as a tool"));
    let state = State::new();
    assert_eq!(
        Hook::of(&case("Claude Code 2.1.323")).bind(&state, "outer", 1000),
        Ok(())
    );
    assert_eq!(
        nested().bind(&state, "nested", 2000),
        Err(BindRefused::NotInGroup)
    );
    assert_eq!(refusal_of(&state), None);
    assert_eq!(binding(&state).unwrap().session, "outer");
    let state = State::new();
    state.bind(&Binding {
        agent: incarnation(26049),
        ..bound_to(26048, "behind a launcher")
    });
    assert_eq!(
        nested().bind(&state, "nested", 2000),
        Err(BindRefused::NotInGroup)
    );
    assert_eq!(refusal_of(&state), None, "the launcher is the foreground");
    let state = State::new();
    state.bind(&bound_to(4242, "an earlier agent"));
    assert_eq!(
        nested().bind(&state, "nested", 2000),
        Err(BindRefused::NotInGroup)
    );
    assert_eq!(
        refusal_of(&state).and_then(|refusal| refusal.foreground),
        Some(incarnation(26048))
    );
}

/// Doctor's read: a refusal counts only under the foreground it was refused
/// under, and one that names another block, or no foreground, never counts.
#[test]
fn doctor_reads_a_refusal_only_under_the_foreground_it_was_refused_under() {
    let state = State::new();
    let under = |foreground: &str| refusal(&state.0, BLOCK, &Incarnation(foreground.into()));
    let write = |value: Value| {
        let path = state.0.join(format!(
            "rex/refusals/{}.json",
            BLOCK.strip_prefix("block:").unwrap()
        ));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, value.to_string()).unwrap();
    };
    let refused = |block: &str, foreground: Option<&str>| {
        serde_json::json!({"block": block, "endpoint": ENDPOINT, "condition": "not_in_group",
                           "session": "nested", "foreground": foreground, "at_ms": 1})
    };
    write(refused(BLOCK, Some("26048@26048000")));
    assert_eq!(
        under("26048@26048000").map(|refusal| refusal.session),
        Some("nested".into())
    );
    assert_eq!(under("26048@99"), None, "a later program in the same pid");
    assert_eq!(under("5000@5000000"), None);
    write(refused(BLOCK, None));
    assert_eq!(under("26048@26048000"), None);
    write(refused(
        "block:11111111-1b5a-4f8e-9d2c-3a7b8c9d0e1f",
        Some("26048@26048000"),
    ));
    assert_eq!(under("26048@26048000"), None, "another block's refusal");
}

#[test]
fn an_identical_bind_is_not_rewritten_and_a_new_session_replaces_the_binding() {
    let state = State::new();
    let hook = Hook::of(&case("Codex 0.163 with --no-daemon"));
    assert_eq!(hook.bind(&state, "session-1", 1000), Ok(()));
    let path = Bindings::new(&state.0).path(BLOCK).unwrap();
    let written = std::fs::metadata(&path).unwrap().modified().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert_eq!(hook.bind(&state, "session-1", 2000), Ok(()));
    assert_eq!(binding(&state).unwrap().bound_at_ms, 1000);
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        written
    );
    assert_eq!(hook.bind(&state, "after-clear", 3000), Ok(()));
    let bound = binding(&state).unwrap();
    assert_eq!(
        (bound.session.as_str(), bound.bound_at_ms),
        ("after-clear", 3000)
    );
}

/// The launcher rule's other half: a program that is not a shell between
/// the foreground and the hook's agent refuses even when the agent runs
/// another executable, as another Claude Code version started by a Python
/// MCP server would (inferred from the measured chains, not run).
#[test]
fn a_program_between_the_foreground_and_another_executable_refuses() {
    let state = State::new();
    let hook = Hook::of(&case(
        "another Claude Code version from a shell MCP server, before",
    ))
    .entrypoint(None);
    hook.local.runs(
        70090,
        "/Library/Frameworks/Python.framework/Versions/3.12/bin/python3",
    );
    assert_eq!(
        hook.bind(&state, "session-1", 1000),
        Err(BindRefused::Launcher)
    );
}

/// macOS cannot read the executable of a process whose file was unlinked
/// while it runs, as an agent's update can leave it. An agent that is the
/// foreground still binds, so its session after `/clear` replaces the one
/// before. Behind a launcher, the rule compares executables, so either one
/// unread refuses.
#[test]
fn an_agent_whose_executable_cannot_be_read_binds_only_as_the_foreground() {
    let unread = |hook: Hook, pid: u32| {
        hook.local.executables.lock().unwrap().remove(&pid);
        hook
    };
    let state = State::new();
    state.bind(&bound_to(26048, "before-clear"));
    let foreground = unread(Hook::of(&case("Claude Code 2.1.323")), 26048);
    assert_eq!(foreground.bind(&state, "after-clear", 2000), Ok(()));
    assert_eq!(binding(&state).unwrap().session, "after-clear");
    let behind = || Hook::of(&case("Claude Code behind bash wrap.sh, SessionStart"));
    for (hook, refused) in [
        (unread(behind(), 82429), BindRefused::NotAlive),
        (unread(behind(), 82428), BindRefused::Launcher),
    ] {
        let state = State::new();
        assert_eq!(hook.bind(&state, "session-1", 1000), Err(refused));
        assert_eq!(binding(&state), None);
    }
}

/// Condition 5 reads the binding under the block's lock: a nested hook that
/// waits while the outer agent binds then sees that bind and refuses,
/// rather than overwriting it with the binding it would have written.
#[test]
fn a_hook_reads_and_writes_the_binding_under_the_blocks_lock() {
    let state = State::new();
    let bindings = Bindings::new(&state.0);
    let nested = Hook::of(&case("Claude Code behind bash wrap.sh, SessionStart"));
    let held = bindings.lock(BLOCK).unwrap();
    std::thread::scope(|scope| {
        let waiting = scope.spawn(|| nested.bind(&state, "nested", 2000));
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(!waiting.is_finished(), "the hook waits for the lock");
        assert_eq!(binding(&state), None);
        state.bind(&bound_to(82428, "outer"));
        drop(held);
        assert_eq!(waiting.join().unwrap(), Err(BindRefused::AlreadyBound));
    });
    assert_eq!(binding(&state).unwrap().session, "outer");

    let state = State::new();
    let outer = Hook::of(&case("Claude Code 2.1.323"));
    let watched = Watched {
        local: &outer.local,
        lock: state.0.join(format!(
            "rex/locks/{}.lock",
            BLOCK.strip_prefix("block:").unwrap()
        )),
        held: std::sync::Mutex::new(Vec::new()),
    };
    assert_eq!(
        outer.place.bind(
            &state.0,
            &AgentSessionId::new("outer"),
            outer.runtime,
            None,
            &watched,
            1000
        ),
        Ok(())
    );
    let held = watched.held.into_inner().unwrap();
    assert!(
        !held.is_empty() && held.iter().all(|held| *held),
        "the lock is held while the hook reads its processes: {held:?}"
    );
}

/// `local`'s processes, recording at each read whether another holder
/// could take the block's lock.
struct Watched<'a> {
    local: &'a Local,
    lock: PathBuf,
    held: std::sync::Mutex<Vec<bool>>,
}

impl LocalProcesses for Watched<'_> {
    fn read(&self, pid: u32) -> LocalRead {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(&self.lock)
            .unwrap();
        // SAFETY: flock only reads the descriptor, which `file` owns.
        let taken = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
        self.held.lock().unwrap().push(!taken);
        self.local.read(pid)
    }

    fn list_group(&self, group: u32) -> Option<Vec<u32>> {
        self.local.list_group(group)
    }

    fn parent(&self, pid: u32) -> Option<u32> {
        self.local.parent(pid)
    }

    fn executable(&self, pid: u32) -> Option<PathBuf> {
        self.local.executable(pid)
    }
}

#[test]
fn a_place_with_no_verified_caller_binds_nothing_and_records_nothing() {
    let state = State::new();
    let mut hook = Hook::of(&case("Claude Code 2.1.323"));
    hook.place.caller = None;
    assert_eq!(
        hook.bind(&state, "session-1", 1000),
        Err(BindRefused::NoCaller)
    );
    assert!(!state.0.join("rex").exists());
}

#[test]
fn the_hooks_agent_is_the_nearest_ancestor_whose_executable_is_not_a_shell() {
    let local = Local::default();
    for (pid, executable) in [
        (1, "/bin/sh"),
        (2, "/usr/bin/dash"),
        (3, "/bin/busybox"),
        (4, "/usr/local/bin/fish"),
        (5, "/opt/codex"),
    ] {
        local.runs(pid, executable);
    }
    assert_eq!(
        hook_agent(&local, &[1, 2, 3, 4, 5, 6], 6),
        Some(HookAgent {
            at: 4,
            pid: 5,
            executable: Some("/opt/codex".into())
        })
    );
    assert_eq!(
        hook_agent(&local, &[1, 7, 5], 5),
        None,
        "an executable that cannot be read cannot be told from a shell"
    );
    assert_eq!(
        hook_agent(&local, &[1, 7, 5], 7),
        Some(HookAgent {
            at: 1,
            pid: 7,
            executable: None
        }),
        "unless it is the foreground"
    );
}

/// A real `#!/bin/sh` wrapper, named as an installed one is, runs this test
/// binary again, which finds its hook agent through the system's own reads.
/// On Linux the wrapper's `comm` is the script's truncated name, and only its
/// executable says it is a shell.
#[test]
fn a_real_shell_wrapper_is_skipped_for_the_process_that_ran_it() {
    const CHILD: &str = "NAMEDROP_HOOK_AGENT_CHILD";
    if let Some(out) = std::env::var_os(CHILD) {
        let ancestors = crate::mux::processes::ancestors(&ThisMachine, std::process::id());
        let agent = hook_agent(&ThisMachine, &ancestors, 0).expect("a hook agent");
        let comm =
            std::fs::read_to_string(format!("/proc/{}/comm", ancestors[0])).unwrap_or_default();
        std::fs::write(
            out,
            serde_json::to_vec(&serde_json::json!({
                "pid": agent.pid, "at": agent.at, "executable": agent.executable,
                "wrapper": ThisMachine.executable(ancestors[0]), "comm": comm.trim(),
            }))
            .unwrap(),
        )
        .unwrap();
        return;
    }
    let state = State::new();
    let wrapper = state.0.join("session-start-wrapper");
    std::fs::write(&wrapper, "#!/bin/sh\n\"$@\"\nstatus=$?\nexit $status\n").unwrap();
    std::fs::set_permissions(
        &wrapper,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let out = state.0.join("agent.json");
    let ran = std::process::Command::new(&wrapper)
        .arg(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "mux::rex::binding::tests::a_real_shell_wrapper_is_skipped_for_the_process_that_ran_it",
            "--nocapture",
        ])
        .env(CHILD, &out)
        .output()
        .unwrap();
    assert!(ran.status.success(), "{ran:?}");
    let found: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(found["pid"], std::process::id(), "{found}");
    assert_eq!(found["at"], 1, "{found}");
    assert_eq!(
        found["executable"],
        Value::from(
            ThisMachine
                .executable(std::process::id())
                .unwrap()
                .to_str()
                .unwrap()
        )
    );
    let shell = found["wrapper"].as_str().unwrap();
    assert!(is_shell(Path::new(shell)), "{found}");
    if cfg!(target_os = "linux") {
        assert_eq!(found["comm"], "session-start-w", "{found}");
    }
}

/// A context hook's process in the block of a measured chain: `REX_*`
/// claims the block on the server at the chain's end, which proves local and
/// shows the chain's foreground and record.
struct InBlock {
    vars: HashMap<String, String>,
    ancestors: Vec<u32>,
    peer: io::Result<u32>,
    caller: Caller,
}

impl InBlock {
    fn of(hook: &Hook) -> Self {
        let caller = hook.place.caller.as_deref().unwrap().clone();
        let server = *caller.ancestors.last().unwrap();
        Self {
            vars: [
                ("REX_BLOCK", BLOCK),
                ("REX_SESSION", "session:s"),
                ("REX_SERVER", "unix:///tmp/r.sock"),
            ]
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .into(),
            ancestors: caller.ancestors.clone(),
            peer: Ok(server),
            caller,
        }
    }

    fn var(mut self, name: &str, value: &str) -> Self {
        self.vars.insert(name.to_owned(), value.to_owned());
        self
    }

    fn hook(
        &self,
        state: &State,
        local: &Local,
        input: &str,
        runtime: Option<AgentRuntime>,
    ) -> HookHome {
        context_hook_on(&state.0, input, runtime, self, local, 1000)
    }
}

impl Inherited for InBlock {
    fn var(&self, name: &str) -> Option<OsString> {
        self.vars.get(name).map(OsString::from)
    }

    fn id(&self) -> u32 {
        1
    }

    fn ancestors(&self) -> Vec<u32> {
        self.ancestors.clone()
    }

    fn peer(&self, socket: &Path) -> io::Result<u32> {
        assert_eq!(socket, Path::new("/tmp/r.sock"));
        match &self.peer {
            Ok(pid) => Ok(*pid),
            Err(error) => Err(io::Error::from_raw_os_error(error.raw_os_error().unwrap())),
        }
    }

    fn claimed(&self, _: &Path, session: &str, block: &str) -> Result<Claimed, String> {
        assert_eq!((session, block), ("session:s", BLOCK));
        Ok(Claimed {
            endpoint: Endpoint::parse(ENDPOINT),
            server: *self.ancestors.last().unwrap(),
            child: self.ancestors[self.ancestors.len() - 2],
            caller: self.caller.clone(),
        })
    }

    fn incarnation(&self, _: u32) -> Option<String> {
        None
    }
}

fn input(session: &str) -> String {
    serde_json::json!({"hook_event_name": "SessionStart", "source": "startup", "session_id": session})
        .to_string()
}

/// A context hook binds the session its host sent to the block it runs in,
/// with the runtime its wrapper named and the entrypoint it inherited, so a
/// print-mode Claude Code behind the foreground refuses. Its notices speak
/// Rex's words, and it records its turn at the block.
#[test]
fn a_context_hook_in_a_rex_block_binds_its_session_with_its_inherited_entrypoint() {
    let hook = Hook::of(&case("Claude Code behind bash wrap.sh, SessionStart"));
    let state = State::new();
    let home = InBlock::of(&hook)
        .var("CLAUDE_CODE_ENTRYPOINT", "cli")
        .hook(
            &state,
            &hook.local,
            &input("outer"),
            Some(AgentRuntime::ClaudeCode),
        );
    assert_eq!(
        home,
        HookHome {
            kind: MuxKind::Rex,
            location: Some(Location {
                endpoint: Some(Endpoint::parse(ENDPOINT)),
                space: Some(SpaceId::new("session:s")),
                pane: Some(PaneId::new(BLOCK)),
                names: crate::locator::Name::Neutral,
            }),
        }
    );
    assert_eq!(binding(&state).unwrap().session, "outer");
    let state = State::new();
    InBlock::of(&hook)
        .var("CLAUDE_CODE_ENTRYPOINT", "sdk-cli")
        .hook(
            &state,
            &hook.local,
            &input("nested"),
            Some(AgentRuntime::ClaudeCode),
        );
    assert_eq!(binding(&state), None);
    assert_eq!(
        refusal_of(&state).map(|refusal| (refusal.condition, refusal.session)),
        Some((BindRefused::NestedPrintMode, "nested".into()))
    );
}

/// A wrapper that names no runtime, or a host that sends no session, binds
/// nothing and leaves nothing for Doctor.
#[test]
fn a_context_hook_without_a_runtime_or_a_session_binds_nothing() {
    let hook = Hook::of(&case("Claude Code 2.1.323"));
    for (input, runtime) in [
        (input("s1"), None),
        (input(""), Some(AgentRuntime::ClaudeCode)),
        (
            r#"{"hook_event_name":"SessionStart"}"#.to_owned(),
            Some(AgentRuntime::ClaudeCode),
        ),
    ] {
        let state = State::new();
        let home = InBlock::of(&hook).hook(&state, &hook.local, &input, runtime);
        assert_eq!(home.kind, MuxKind::Rex);
        assert!(!state.0.join("rex").exists(), "{input} {runtime:?}");
    }
}

/// A Herdr pane's context hook speaks Herdr's words and writes nothing under
/// `rex/`, with or without its wrapper's runtime.
#[test]
fn a_context_hook_in_a_herdr_pane_speaks_herdr_and_binds_nothing() {
    let hook = Hook::of(&case("Claude Code 2.1.323"));
    let mut herdr = InBlock::of(&hook).var("HERDR_PANE_ID", "w1:p1");
    herdr.vars.retain(|name, _| !name.starts_with("REX_"));
    for runtime in [Some(AgentRuntime::ClaudeCode), None] {
        let state = State::new();
        let home = herdr.hook(&state, &hook.local, &input("s1"), runtime);
        assert_eq!(home.kind, MuxKind::Herdr);
        assert_eq!(
            home.location.map(|location| location.pane),
            Some(Some(PaneId::new("w1:p1")))
        );
        assert!(!state.0.join("rex").exists());
    }
}

/// A hook whose Rex claim cannot be resolved, as one under a Codex app
/// server whose Codex exited, finds the claimed server outside its
/// ancestors, or no server at all. It is still in a Rex block, so its
/// notices speak Rex's words, and it binds nothing and records its turn
/// nowhere.
#[test]
fn a_context_hook_whose_rex_claim_fails_still_speaks_rex() {
    let hook = Hook::of(&case("Codex 0.163 with --no-daemon"));
    for peer in [Ok(99_999), Err(io::Error::from_raw_os_error(libc::ENOENT))] {
        let mut lost = InBlock::of(&hook);
        lost.ancestors = vec![1];
        lost.peer = peer;
        let state = State::new();
        assert_eq!(
            lost.hook(&state, &hook.local, &input("s1"), Some(AgentRuntime::Codex)),
            HookHome {
                kind: MuxKind::Rex,
                location: None,
            }
        );
        assert!(!state.0.join("rex").exists());
    }
}
