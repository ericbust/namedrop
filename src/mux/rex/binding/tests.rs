use super::*;
use crate::{
    SpaceId,
    mux::{
        Endpoint, PaneId,
        rex::tests::{Local, State},
    },
};
use serde_json::Value;

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
    refusal(&state.0, BLOCK)
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
        hook_agent(&local, &[1, 2, 3, 4, 5, 6]),
        Some(HookAgent {
            at: 4,
            pid: 5,
            executable: "/opt/codex".into()
        })
    );
    assert_eq!(
        hook_agent(&local, &[1, 7, 5]),
        None,
        "an executable that cannot be read cannot be told from a shell"
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
        let agent = hook_agent(&ThisMachine, &ancestors).expect("a hook agent");
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
