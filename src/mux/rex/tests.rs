use super::{
    agent::{Locality, Observed, PaneAgent, pane_agent, shell_of},
    binding::Binding,
    control::{Control, Event, Fault, Narrower, Reply, Wire},
    wire::{ProcessReply, StatusRecord, TERMINAL, port_ansi, root_record},
    *,
};
use crate::{
    AgentRuntime, AgentSessionId,
    mux::{
        AgentIdentity, AgentKind, AgentState, Incarnation, SessionClaim,
        processes::{LocalProc, LocalRead},
    },
};
use std::{
    collections::{HashMap, VecDeque},
    sync::atomic::{AtomicU32, Ordering},
};

pub(super) const PEER: u32 = 4680;
pub(super) const HOST: &str = "0e9d6c1f-4a3b-4a52-9c85-2f6d3f6f2f10";
pub(super) const SOCKET: &str = "/tmp/r.sock";

/// One request a scripted server received.
pub(super) struct Request {
    pub method: String,
    pub payload: Value,
}

impl Request {
    /// The method without the terminal creator's prefix.
    pub fn short(&self) -> &str {
        self.method
            .strip_prefix(TERMINAL)
            .and_then(|method| method.strip_prefix('.'))
            .unwrap_or(&self.method)
    }

    /// The request as the tests pin it: the short method, then what it names.
    fn line(&self) -> String {
        let payload = &self.payload;
        let named = match self.short() {
            "target.resolve" => payload["queries"][0].as_str().map(str::to_owned),
            "block.event" => Some(match payload["event"]["type"].as_str() {
                Some("paste") => format!(
                    "paste {}",
                    payload["event"]["payload"]["text"].as_str().unwrap_or("")
                ),
                Some("key") => format!(
                    "key {}",
                    payload["event"]["payload"]["key"].as_str().unwrap_or("")
                ),
                other => format!("{other:?}"),
            }),
            "session.set_block_label" => Some(format!(
                "{} {}",
                payload["block_id"].as_str().unwrap_or(""),
                payload["label"].as_str().unwrap_or("")
            )),
            "session.set_window_label" => Some(format!(
                "{} {}",
                payload["window_id"].as_str().unwrap_or(""),
                payload["label"].as_str().unwrap_or("")
            )),
            _ => payload["block_id"]
                .as_str()
                .or(payload["session_id"].as_str())
                .map(str::to_owned),
        };
        match named {
            Some(named) => format!("{} {named}", self.short()),
            None => self.short().to_owned(),
        }
    }
}

/// Events a handler queues while it answers a request.
#[derive(Default)]
pub(super) struct Events {
    earlier: Vec<Value>,
    later: Vec<Value>,
}

impl Events {
    /// Queued before the server started the request, so the reply's `seq`
    /// counts it.
    pub fn earlier(&mut self, event: Value) {
        self.earlier.push(event);
    }

    /// Queued after the server started the request.
    pub fn later(&mut self, event: Value) {
        self.later.push(event);
    }
}

type Handler = dyn Fn(&Request, &mut Events) -> Result<Value, Fault> + Send + Sync;

/// A Rex server that answers each request with `handler` and records it.
pub(super) struct Script {
    handler: Box<Handler>,
    pub peer: AtomicU32,
    /// What `Wire::timeout` answers, in milliseconds.
    pub timeout_ms: AtomicU32,
    log: Mutex<Vec<String>>,
    opened: AtomicU32,
}

impl Script {
    pub fn new(
        handler: impl Fn(&Request, &mut Events) -> Result<Value, Fault> + Send + Sync + 'static,
    ) -> Self {
        Self {
            handler: Box::new(handler),
            peer: AtomicU32::new(PEER),
            timeout_ms: AtomicU32::new(30_000),
            log: Mutex::default(),
            opened: AtomicU32::new(0),
        }
    }

    /// What the handler answers, for a script that wraps another.
    pub fn answer(&self, request: &Request, events: &mut Events) -> Result<Value, Fault> {
        (self.handler)(request, events)
    }

    pub fn calls(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    pub fn forget(&self) {
        self.log.lock().unwrap().clear();
    }
}

struct Scripted<'a> {
    script: &'a Script,
    seq: u64,
    queued: VecDeque<Event>,
}

impl Control for Scripted<'_> {
    fn request(&mut self, method: &str, payload: Option<Value>) -> Result<Reply, Fault> {
        let request = Request {
            method: method.to_owned(),
            payload: payload.unwrap_or(Value::Null),
        };
        self.script.log.lock().unwrap().push(request.line());
        let mut events = Events::default();
        let answer = (self.script.handler)(&request, &mut events);
        for payload in events.earlier {
            self.seq += 1;
            self.queued.push_back(Event {
                seq: self.seq,
                payload,
            });
        }
        let seq = self.seq;
        for payload in events.later {
            self.seq += 1;
            self.queued.push_back(Event {
                seq: self.seq,
                payload,
            });
        }
        answer.map(|payload| Reply { seq, payload })
    }

    fn next_event(&mut self, _within: Duration) -> Result<Option<Event>, Fault> {
        Ok(self.queued.pop_front())
    }
}

impl Wire for Script {
    fn open<'a>(&'a self, narrower: Narrower<'a>) -> Result<(Box<dyn Control + 'a>, u32), Fault> {
        if narrower
            .cancelled
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
        {
            return Err(Fault::Cancelled);
        }
        (narrower.budget)().map_err(Fault::Allowance)?;
        self.opened.fetch_add(1, Ordering::Relaxed);
        let control = Scripted {
            script: self,
            seq: 0,
            queued: VecDeque::new(),
        };
        Ok((Box::new(control), self.peer.load(Ordering::Relaxed)))
    }

    fn timeout(&self) -> Result<Duration, String> {
        Ok(Duration::from_millis(u64::from(
            self.timeout_ms.load(Ordering::Relaxed),
        )))
    }
}

/// Local processes as a table: each pid's read, and each group's listing,
/// which holds zombies as the system's listing does.
#[derive(Default)]
pub(super) struct Local {
    pub reads: Mutex<HashMap<u32, LocalRead>>,
    pub groups: Mutex<HashMap<u32, Vec<u32>>>,
    /// A remote block's pids name nothing here, so reading one fails the test.
    pub forbidden: bool,
}

impl Local {
    pub fn alive(&self, pid: u32, start: u64, group: u32) {
        self.reads.lock().unwrap().insert(
            pid,
            LocalRead::Alive(LocalProc {
                incarnation: Incarnation(format!("{pid}@{start}")),
                group,
            }),
        );
        self.groups
            .lock()
            .unwrap()
            .entry(group)
            .or_default()
            .push(pid);
    }

    pub fn set(&self, pid: u32, read: LocalRead) {
        self.reads.lock().unwrap().insert(pid, read);
    }

    /// The fixture's local facts: a live pid starts at `<pid>000`, a zombie
    /// stays in its group's listing, and a gone pid is in none.
    pub fn from_fixture(fixture: &Value) -> Self {
        let local = Self::default();
        for (pid, facts) in fixture["local"].as_object().unwrap() {
            let pid: u32 = pid.parse().unwrap();
            let group = u32::try_from(facts["group"].as_u64().unwrap()).unwrap();
            match facts["state"].as_str().unwrap() {
                "alive" => local.alive(pid, u64::from(pid) * 1000, group),
                "zombie" => {
                    local.set(pid, LocalRead::Gone);
                    local
                        .groups
                        .lock()
                        .unwrap()
                        .entry(group)
                        .or_default()
                        .push(pid);
                }
                _ => local.set(pid, LocalRead::Gone),
            }
        }
        local
    }
}

impl LocalProcesses for Local {
    fn read(&self, pid: u32) -> LocalRead {
        assert!(
            !self.forbidden,
            "process {pid} was read for a block on another machine"
        );
        self.reads
            .lock()
            .unwrap()
            .get(&pid)
            .cloned()
            .unwrap_or(LocalRead::Gone)
    }

    fn list_group(&self, group: u32) -> Option<Vec<u32>> {
        assert!(
            !self.forbidden,
            "group {group} was listed for a block on another machine"
        );
        self.groups.lock().unwrap().get(&group).cloned()
    }

    fn parent(&self, _pid: u32) -> Option<u32> {
        None
    }

    fn executable(&self, _pid: u32) -> Option<PathBuf> {
        None
    }
}

pub(super) fn fixture(name: &str) -> Value {
    let text = match name {
        "idle-login" => include_str!("fixtures/process/idle-login.json"),
        "busy-login" => include_str!("fixtures/process/busy-login.json"),
        "python-interactive" => include_str!("fixtures/process/python-interactive.json"),
        "bash-c" => include_str!("fixtures/process/bash-c.json"),
        "stale-before" => include_str!("fixtures/process/stale-before.json"),
        "stale-after" => include_str!("fixtures/process/stale-after.json"),
        "wrapper-before" => include_str!("fixtures/process/wrapper-before.json"),
        "wrapper-after" => include_str!("fixtures/process/wrapper-after.json"),
        "zombie-launcher" => include_str!("fixtures/process/zombie-launcher.json"),
        other => panic!("no fixture {other}"),
    };
    serde_json::from_str(text).unwrap()
}

pub(super) fn status(pid: u32) -> Value {
    json!({
        "pid": pid,
        "host_id": HOST,
        "listeners": [{"endpoint": format!("unix://{SOCKET}"), "addresses": [SOCKET],
                       "transport": "unix", "primary": true}],
    })
}

pub(super) fn rex_error(code: &str) -> Fault {
    Fault::Rex {
        code: code.into(),
        message: format!("Rex says {code}"),
        status: 404,
    }
}

/// A server with one session `session:s` holding the terminal block
/// `block:b` in `window:w`, whose `program_status` and `process` answer as
/// `fixture` shows.
pub(super) fn one_block(fixture: Value) -> Script {
    Script::new(move |request, _| match request.short() {
        "server.status" => Ok(status(PEER)),
        "session.list" => Ok(json!({"sessions": [
            {"session_id": "session:s", "label": "work"},
        ]})),
        "session.attach" => Ok(json!({"view": {"windows": [{"layers": [{"blocks": [
            {"block_id": "block:b", "host_id": HOST},
        ]}]}]}})),
        "session.list_blocks" => Ok(json!({"blocks": [
            {"block_id": "block:b", "label": "agent", "creator_name": TERMINAL,
             "host_id": HOST, "placement": "placed", "window_id": "window:w"},
        ]})),
        "target.resolve" => Ok(json!({"results": [{"query": request.payload["queries"][0],
            "status": "resolved", "target": {"kind": "block", "session_id": "session:s",
            "window_id": "window:w", "block_id": "block:b", "label": "agent"}}]})),
        "program_status" => Ok(fixture["program_status"].clone()),
        "process" => Ok(fixture["process"].clone()),
        _ => Err(rex_error("not_found")),
    })
}

pub(super) struct State(pub PathBuf);

impl State {
    pub fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "nd-rex-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    pub fn bind(&self, binding: &Binding) {
        let path = Bindings::new(&self.0).path(&binding.block).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec(binding).unwrap()).unwrap();
    }
}

impl Drop for State {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(super) fn rex<W: Wire>(wire: W, state: &State, local: Local) -> RexMux<W> {
    rex_shared(wire, state, Arc::new(local))
}

/// `rex` with processes the test keeps a handle on, to change them mid-call.
pub(super) fn rex_shared<W: Wire>(wire: W, state: &State, local: Arc<Local>) -> RexMux<W> {
    RexMux::new(
        wire,
        Endpoint::parse(&format!("rex:{SOCKET}")),
        &state.0,
        local,
    )
}

/// A binding of `block:b` to `session-1`, whose foreground and agent are
/// the incarnations given.
pub(super) fn binding(foreground: &str, agent: &str, runtime: &str) -> Binding {
    Binding {
        block: "block:b".into(),
        endpoint: format!("rex:{SOCKET}"),
        space: "session:s".into(),
        foreground: foreground.into(),
        agent: agent.into(),
        runtime: runtime.into(),
        session: "session-1".into(),
        bound_at_ms: 0,
    }
}

fn claude(session: SessionClaim, state: &str) -> PaneAgent {
    PaneAgent {
        reports_session: matches!(session, SessionClaim::Reported(_)),
        agent: Some(AgentIdentity {
            kind: AgentKind::Known(AgentRuntime::ClaudeCode),
            label: Some("claude-code".into()),
            session,
        }),
        state: Some(AgentState::parse(state)),
    }
}

fn free() -> PaneAgent {
    PaneAgent {
        agent: None,
        state: None,
        reports_session: true,
    }
}

fn reported() -> SessionClaim {
    SessionClaim::Reported(AgentSessionId::new("session-1"))
}

/// `pane_agent` for a fixture: its root record and foreground, a binding,
/// and the locality read from its local facts, as the survey reads them.
fn observe(
    fixture: &Value,
    binding: Option<&Binding>,
    local: &Local,
    host: Option<&str>,
) -> PaneAgent {
    let record: Option<StatusRecord> = root_record(fixture["program_status"].clone()).unwrap();
    let process: ProcessReply = serde_json::from_value(fixture["process"].clone()).unwrap();
    let locality = Locality::read(
        local,
        host,
        Some(HOST),
        process.foreground.as_ref(),
        binding,
    );
    pane_agent(Observed {
        record: record.as_ref(),
        foreground: process.foreground.as_ref(),
        binding,
        locality,
    })
}

#[test]
fn a_record_whose_owner_is_the_live_foreground_bound_to_its_session_reports_it() {
    let fixture = fixture("stale-before");
    let local = Local::from_fixture(&fixture);
    let bound = binding("75361@75361000", "75361@75361000", "claude-code");
    assert_eq!(
        observe(&fixture, Some(&bound), &local, Some(HOST)),
        claude(reported(), "idle")
    );
}

#[test]
fn an_unbound_agent_or_one_bound_to_another_incarnation_is_unreported() {
    let fixture = fixture("stale-before");
    let local = Local::from_fixture(&fixture);
    let unreported = claude(
        SessionClaim::Unreported {
            incarnation: Incarnation("75361@75361000".into()),
        },
        "idle",
    );
    assert_eq!(observe(&fixture, None, &local, Some(HOST)), unreported);
    let older = binding("75361@1", "75361@1", "claude-code");
    assert_eq!(
        observe(&fixture, Some(&older), &local, Some(HOST)),
        unreported
    );
}

#[test]
fn a_launcher_speaks_for_its_bound_agent_only_while_the_agent_lives_in_its_group() {
    let before = fixture("wrapper-before");
    let bound = binding("70962@70962000", "70969@70969000", "claude-code");
    let local = Local::from_fixture(&before);
    assert_eq!(
        observe(&before, Some(&bound), &local, Some(HOST)),
        claude(reported(), "idle"),
        "the agent behind the wrapper lives"
    );

    let after = fixture("wrapper-after");
    let local = Local::from_fixture(&after);
    assert_eq!(
        observe(&after, Some(&bound), &local, Some(HOST)),
        free(),
        "the agent is gone behind a live launcher"
    );

    let zombie = fixture("zombie-launcher");
    let local = Local::from_fixture(&zombie);
    let behind = binding("71017@71017000", "71031@71031000", "claude-code");
    assert_eq!(
        observe(&zombie, Some(&behind), &local, Some(HOST)),
        free(),
        "the agent is a zombie behind a launcher that never reaps it"
    );

    let local = Local::from_fixture(&before);
    local.set(70969, LocalRead::Unreadable);
    assert_eq!(
        observe(&before, Some(&bound), &local, Some(HOST)),
        free(),
        "an unreadable agent behind a launcher"
    );

    for (why, read) in [
        ("a restarted agent", (70969, 1, 70962)),
        ("an agent that left the group", (70969, 70_969_000, 99)),
    ] {
        let local = Local::from_fixture(&before);
        local.alive(read.0, read.1, read.2);
        assert_eq!(
            observe(&before, Some(&bound), &local, Some(HOST)),
            free(),
            "{why}"
        );
    }
}

#[test]
fn a_binding_whose_runtime_is_not_the_records_app_is_a_free_terminal() {
    let fixture = fixture("stale-before");
    let local = Local::from_fixture(&fixture);
    let codex = binding("75361@75361000", "75361@75361000", "codex");
    assert_eq!(observe(&fixture, Some(&codex), &local, Some(HOST)), free());
}

#[test]
fn an_unreadable_foreground_or_a_remote_block_has_a_kind_and_no_session() {
    let fixture = fixture("stale-before");
    let bound = binding("75361@75361000", "75361@75361000", "claude-code");
    let local = Local::from_fixture(&fixture);
    local.set(75361, LocalRead::Unreadable);
    assert_eq!(
        observe(&fixture, Some(&bound), &local, Some(HOST)),
        claude(SessionClaim::None, "idle")
    );
    let remote = Local {
        forbidden: true,
        ..Local::default()
    };
    assert_eq!(
        observe(&fixture, Some(&bound), &remote, Some("another-host")),
        claude(SessionClaim::None, "idle")
    );
    assert_eq!(
        observe(&fixture, Some(&bound), &remote, None),
        claude(SessionClaim::None, "idle"),
        "a block with no host_id is not known to be here"
    );
}

#[test]
fn a_stale_record_or_a_gone_foreground_is_a_free_terminal() {
    let after = fixture("stale-after");
    let local = Local::from_fixture(&after);
    let bound = binding("75361@75361000", "75361@75361000", "claude-code");
    assert_eq!(
        observe(&after, Some(&bound), &local, Some(HOST)),
        free(),
        "the record names the dead agent while zsh holds the terminal"
    );

    let before = fixture("stale-before");
    let local = Local::from_fixture(&before);
    local.set(75361, LocalRead::Gone);
    assert_eq!(
        observe(&before, Some(&bound), &local, Some(HOST)),
        free(),
        "a fresh record whose foreground is gone"
    );

    let mut reused = before.clone();
    reused["process"]["foreground"]["name"] = json!("vim");
    let local = Local::from_fixture(&before);
    assert_eq!(
        observe(&reused, Some(&bound), &local, Some(HOST)),
        free(),
        "the record's owner pid now runs another program"
    );

    let mut cargo = before.clone();
    cargo["program_status"]["records"][0]["app"] = json!("cargo");
    let local = Local::from_fixture(&before);
    assert_eq!(observe(&cargo, Some(&bound), &local, Some(HOST)), free());
}

#[test]
fn a_bound_agent_with_no_record_yet_reports_its_session_and_no_kind() {
    let mut fixture = fixture("stale-before");
    fixture["program_status"] = json!({"records": []});
    let local = Local::from_fixture(&fixture);
    let bound = binding("75361@75361000", "75361@75361000", "claude-code");
    assert_eq!(
        observe(&fixture, Some(&bound), &local, Some(HOST)),
        PaneAgent {
            agent: Some(AgentIdentity {
                kind: AgentKind::Unreported,
                label: None,
                session: reported(),
            }),
            state: None,
            reports_session: true,
        }
    );
    assert_eq!(observe(&fixture, None, &local, Some(HOST)), free());
}

#[test]
fn shell_of_finds_a_shell_only_below_login_or_as_an_interactive_child() {
    let shell = |name: &str| {
        let fixture = fixture(name);
        let local = Local::from_fixture(&fixture);
        let process: ProcessReply = serde_json::from_value(fixture["process"].clone()).unwrap();
        let foreground = process.foreground.as_ref().unwrap().pid;
        let group = match local.read(foreground) {
            LocalRead::Alive(foreground) => foreground.group,
            other => panic!("{other:?}"),
        };
        shell_of(&process, &members(&local, group).unwrap())
    };
    assert_eq!(shell("idle-login"), Some(70802));
    assert_eq!(shell("busy-login"), Some(70829));
    assert_eq!(shell("python-interactive"), None);
    assert_eq!(shell("bash-c"), None, "sleep runs in bash's own group");
    assert_eq!(
        shell("wrapper-after"),
        None,
        "the wrapper runs sleep in its group"
    );
}

#[test]
fn a_shell_whose_ancestors_rex_left_out_is_not_found() {
    let fixture = fixture("busy-login");
    let mut process: ProcessReply = serde_json::from_value(fixture["process"].clone()).unwrap();
    process.ancestors = None;
    assert_eq!(shell_of(&process, &[75319]), None);
}

#[test]
fn port_ansi_turns_blank_runs_into_spaces_and_refuses_what_the_reader_would_misread() {
    for vt in [
        include_str!("fixtures/vt/claude-empty.vt"),
        include_str!("fixtures/vt/claude-placeholder.vt"),
        include_str!("fixtures/vt/claude-draft.vt"),
        include_str!("fixtures/vt/codex-empty.vt"),
        include_str!("fixtures/vt/codex-draft.vt"),
        include_str!("fixtures/vt/codex-draft-fresh.vt"),
    ] {
        let screen = port_ansi(vt).unwrap();
        assert!(!screen.contains("C\u{1b}") && !screen.contains("\u{1b}[1C"));
        assert_eq!(screen.matches('\n').count(), vt.matches('\n').count());
    }
    assert_eq!(
        port_ansi("\u{203a}\u{1b}[5Cdraft\u{1b}[C!").unwrap(),
        "\u{203a}     draft !",
        "a tab run and a single blank"
    );
    assert_eq!(
        port_ansi("\u{1b}[0m\u{1b}[38;2;1;2;3mhi\u{1b}[38:5:9m\r\n").unwrap(),
        "\u{1b}[0m\u{1b}[38;2;1;2;3mhi\u{1b}[38:5:9m\n"
    );
    let link = "\u{1b}]8;;https://example.com\u{1b}\\link\u{1b}]8;;\u{7}";
    assert_eq!(port_ansi(link).unwrap(), link);
    for (screen, refused) in [
        ("\u{1b}[2Jgone", "\"\\u{1b}[2J\""),
        ("\u{1b}[?25l", "\"\\u{1b}[?25l\""),
        ("\u{1b}]0;title\u{7}", "\"\\u{1b}]0;title\\u{7}\""),
        ("a\tb", "\"\\t\""),
        ("a\rb", "\"\\r\""),
        ("\u{1b}(B", "\"\\u{1b}(\""),
        ("\u{1b}[99999C", "\"\\u{1b}[99999C\""),
        ("cut \u{1b}[3", "\"\\u{1b}[3\""),
    ] {
        assert_eq!(port_ansi(screen), Err(refused.to_owned()), "{screen:?}");
    }
}

#[test]
fn every_survey_pins_its_requests() {
    let state = State::new();
    for (scope, depth, requests) in [
        (
            Scope::All,
            Depth::Spaces,
            vec!["server.status", "session.list"],
        ),
        (
            Scope::Space(&SpaceId::new("session:s")),
            Depth::Spaces,
            vec!["server.status", "session.list"],
        ),
        (
            Scope::Space(&SpaceId::new("session:s")),
            Depth::Panes,
            vec![
                "server.status",
                "session.list",
                "session.attach session:s",
                "session.list_blocks session:s",
                "program_status block:b",
                "process block:b",
            ],
        ),
        (
            Scope::PanesOf(&SpaceId::new("session:s")),
            Depth::Spaces,
            vec![
                "server.status",
                "session.attach session:s",
                "session.list_blocks session:s",
                "program_status block:b",
                "process block:b",
            ],
        ),
        (
            Scope::PanesOf(&SpaceId::new("session:s")),
            Depth::Panes,
            vec![
                "server.status",
                "session.attach session:s",
                "session.list_blocks session:s",
                "program_status block:b",
                "process block:b",
            ],
        ),
        (
            Scope::All,
            Depth::Panes,
            vec![
                "server.status",
                "session.list",
                "session.attach session:s",
                "session.list_blocks session:s",
                "program_status block:b",
                "process block:b",
            ],
        ),
    ] {
        let fixture = fixture("idle-login");
        let rex = rex(
            one_block(fixture.clone()),
            &state,
            Local::from_fixture(&fixture),
        );
        rex.survey(scope, depth).unwrap();
        assert_eq!(rex.wire.calls(), requests, "{scope:?} {depth:?}");
    }
    let fixture = fixture("idle-login");
    let rex = rex(
        one_block(fixture.clone()),
        &state,
        Local::from_fixture(&fixture),
    );
    assert_eq!(
        rex.survey(Scope::CheckoutOf(Path::new("/repo")), Depth::Panes),
        Err(MuxError::Unavailable(
            "a Rex session is not a checkout".into()
        ))
    );
    assert!(rex.wire.calls().is_empty());
}

#[test]
fn a_survey_maps_a_block_to_a_pane_and_a_session_to_a_space() {
    let state = State::new();
    let fixture = fixture("stale-before");
    state.bind(&binding("75361@75361000", "75361@75361000", "claude-code"));
    let rex = rex(
        one_block(fixture.clone()),
        &state,
        Local::from_fixture(&fixture),
    );
    let survey = rex
        .survey(Scope::Space(&SpaceId::new("session:s")), Depth::Panes)
        .unwrap();
    let space = &survey.spaces[0];
    assert_eq!(
        (
            space.id.as_str(),
            space.label.as_deref(),
            space.number,
            space.checkout.as_ref()
        ),
        ("session:s", Some("work"), Some(1), None)
    );
    assert_eq!(space.worktree, Some(WorktreeId::new("rex-space:session:s")));
    assert_eq!(
        space.panes,
        [PaneFacts {
            id: PaneId::new("block:b"),
            terminal: TerminalId::new("block:b"),
            tab: TabId::new("window:w"),
            space: SpaceId::new("session:s"),
            cwd: Some("/private/tmp".into()),
            title: Some("agent".into()),
            agent: claude(reported(), "idle").agent,
            state: Some(AgentState::Idle),
            idle_label: None,
            viewport_rows: None,
            reports_session: true,
        }]
    );
}

#[test]
fn a_survey_never_reads_the_processes_of_a_block_on_another_machine() {
    let state = State::new();
    let fixture = fixture("stale-before");
    let script = Script::new(move |request, events| {
        if request.short() == "session.list_blocks" {
            return Ok(json!({"blocks": [{"block_id": "block:b", "label": "remote",
                "creator_name": TERMINAL, "host_id": "another-host", "window_id": "window:w"}]}));
        }
        (one_block(fixture.clone()).handler)(request, events)
    });
    let rex = rex(
        script,
        &state,
        Local {
            forbidden: true,
            ..Local::default()
        },
    );
    let survey = rex
        .survey(Scope::PanesOf(&SpaceId::new("session:s")), Depth::Panes)
        .unwrap();
    let pane = &survey.spaces[0].panes[0];
    assert_eq!(pane.agent, claude(SessionClaim::None, "idle").agent);
    assert!(!pane.reports_session);
}

#[test]
fn a_survey_skips_blocks_that_are_not_terminals() {
    let state = State::new();
    let fixture = fixture("idle-login");
    let script = Script::new(move |request, events| {
        if request.short() == "session.list_blocks" {
            return Ok(json!({"blocks": [{"block_id": "block:o", "label": "",
                "creator_name": "com.superlogical.onboarding", "host_id": HOST}]}));
        }
        (one_block(fixture.clone()).handler)(request, events)
    });
    let rex = rex(script, &state, Local::default());
    let survey = rex
        .survey(Scope::PanesOf(&SpaceId::new("session:s")), Depth::Panes)
        .unwrap();
    assert!(survey.spaces[0].panes.is_empty());
    assert!(!rex.wire.calls().iter().any(|call| call.contains("block:o")));
}

#[test]
fn a_space_rex_does_not_list_is_gone() {
    let state = State::new();
    let fixture = fixture("idle-login");
    let rex = rex(
        one_block(fixture.clone()),
        &state,
        Local::from_fixture(&fixture),
    );
    assert!(matches!(
        rex.survey(Scope::Space(&SpaceId::new("session:other")), Depth::Spaces),
        Err(MuxError::Gone(Gone::Space, _))
    ));
}

#[test]
fn the_server_is_proved_once_and_again_after_its_pid_changes() {
    let state = State::new();
    let fixture = fixture("idle-login");
    let rex = rex(
        one_block(fixture.clone()),
        &state,
        Local::from_fixture(&fixture),
    );
    rex.survey(Scope::All, Depth::Spaces).unwrap();
    rex.wire.forget();
    rex.survey(Scope::All, Depth::Spaces).unwrap();
    assert_eq!(
        rex.wire.calls(),
        ["session.list"],
        "the same peer is not proved again"
    );
    rex.wire.peer.store(PEER + 1, Ordering::Relaxed);
    rex.wire.forget();
    assert!(matches!(
        rex.survey(Scope::All, Depth::Spaces),
        Err(MuxError::Unavailable(why)) if why.contains("is not a Rex server on this machine")
    ));
    assert_eq!(rex.wire.calls(), ["server.status"]);
}

#[test]
fn a_process_read_finds_the_shell_and_the_live_members_of_the_foreground_group() {
    let state = State::new();
    let fixture = fixture("idle-login");
    let local = Local::from_fixture(&fixture);
    // A zombie the system still lists in the shell's group.
    local
        .groups
        .lock()
        .unwrap()
        .get_mut(&70802)
        .unwrap()
        .push(424_242);
    local.set(424_242, LocalRead::Gone);
    let rex = rex(one_block(fixture), &state, local);
    assert_eq!(
        rex.process(&PaneId::new("block:b")).unwrap(),
        PaneProcess {
            shell_pid: Some(70802),
            foreground_group: Some(70802),
            foreground_pids: vec![70802],
            tty: None,
        }
    );
    assert_eq!(
        rex.wire.calls(),
        [
            "server.status",
            "target.resolve block:b",
            "session.attach session:s",
            "process block:b"
        ]
    );
}

#[test]
fn a_process_read_of_a_remote_block_names_no_shell_and_reads_nothing_here() {
    let state = State::new();
    let fixture = fixture("idle-login");
    let script = Script::new(move |request, events| {
        if request.short() == "session.attach" {
            return Ok(json!({"view": {"windows": [{"layers": [{"blocks": [
                {"block_id": "block:b", "host_id": "another-host"}]}]}]}}));
        }
        (one_block(fixture.clone()).handler)(request, events)
    });
    let rex = rex(
        script,
        &state,
        Local {
            forbidden: true,
            ..Local::default()
        },
    );
    assert_eq!(
        rex.process(&PaneId::new("block:b")).unwrap(),
        PaneProcess::default()
    );
}

#[test]
fn a_detached_block_is_found_in_the_block_list() {
    let state = State::new();
    let fixture = fixture("idle-login");
    let script = Script::new(move |request, events| {
        if request.short() == "session.attach" {
            return Ok(json!({"view": {"windows": []}}));
        }
        (one_block(fixture.clone()).handler)(request, events)
    });
    let local = Local::from_fixture(&self::fixture("idle-login"));
    let rex = rex(script, &state, local);
    assert_eq!(
        rex.process(&PaneId::new("block:b")).unwrap().shell_pid,
        Some(70802)
    );
    assert!(
        rex.wire
            .calls()
            .contains(&"session.list_blocks session:s".to_owned())
    );
}

fn screen_script(content: &'static str) -> Script {
    Script::new(move |request, _| match request.short() {
        "server.status" => Ok(status(PEER)),
        "target.resolve" => Ok(
            json!({"results": [{"query": "block:b", "status": "resolved",
            "target": {"kind": "block", "session_id": "session:s", "block_id": "block:b", "label": ""}}]}),
        ),
        "session.attach" => Ok(json!({"view": {"windows": []}})),
        "format" => Ok(json!({"content": &content[..1], "id": 7, "total": content.len()})),
        "blob_read" => {
            let args = &request.payload["args"];
            assert_eq!(args["id"], 7);
            let offset = usize::try_from(args["offset"].as_u64().unwrap()).unwrap();
            Ok(json!({"id": 7, "data": &content[offset..], "total": content.len(), "eof": true}))
        }
        _ => Err(rex_error("not_found")),
    })
}

fn ansi(lines: usize) -> ScreenRequest {
    ScreenRequest {
        lines,
        wrap: Wrap::AsDrawn,
        style: Style::Ansi,
    }
}

#[test]
fn a_screen_read_takes_the_tail_in_the_ports_shape() {
    let state = State::new();
    let rex = rex(
        screen_script("old\r\n\u{203a}\u{1b}[3Cdraft\r\nlast"),
        &state,
        Local::default(),
    );
    assert_eq!(
        rex.screen(&PaneId::new("block:b"), ansi(2)).unwrap().text,
        "\u{203a}   draft\nlast"
    );
    assert_eq!(
        rex.wire.calls(),
        [
            "server.status",
            "target.resolve block:b",
            "session.attach session:s",
            "format block:b",
            "blob_read block:b"
        ]
    );
}

#[test]
fn a_screen_read_from_inside_a_long_result_drops_the_cut_line() {
    let state = State::new();
    let long: &'static str = Box::leak(
        format!(
            "{}\nsecond\nthird",
            "x".repeat(usize::try_from(SCREEN_TAIL).unwrap() + 10)
        )
        .into_boxed_str(),
    );
    let rex = rex(screen_script(long), &state, Local::default());
    assert_eq!(
        rex.screen(&PaneId::new("block:b"), ansi(60)).unwrap().text,
        "second\nthird"
    );
}

#[test]
fn a_screen_the_reader_would_misread_is_a_fault_that_names_the_sequence() {
    let state = State::new();
    let rex = rex(screen_script("box\u{1b}[2Jhere"), &state, Local::default());
    assert_eq!(
        rex.screen(&PaneId::new("block:b"), ansi(5)),
        Err(MuxError::Unavailable(
            "Rex's screen holds an escape sequence Namedrop does not read: \"\\u{1b}[2J\"".into()
        ))
    );
}

#[test]
fn a_tab_label_is_the_windows_label() {
    let state = State::new();
    let script = Script::new(|request, _| match request.short() {
        "server.status" => Ok(status(PEER)),
        "target.resolve" if request.payload["kinds"] == json!(["window"]) => {
            let label = match request.payload["queries"][0].as_str() {
                Some("window:w") => "build",
                _ => "",
            };
            Ok(
                json!({"results": [{"query": request.payload["queries"][0], "status": "resolved",
                "target": {"kind": "window", "session_id": "session:s", "label": label}}]}),
            )
        }
        "target.resolve" => {
            Ok(json!({"results": [{"status": "not_found", "query": "", "reason": ""}]}))
        }
        _ => Err(rex_error("not_found")),
    });
    let rex = rex(script, &state, Local::default());
    assert_eq!(
        rex.tab_label(&TabId::new("window:w")),
        Ok(Some("build".into()))
    );
    assert_eq!(rex.tab_label(&TabId::new("window:x")), Ok(None));
    assert_eq!(
        rex.wire.calls(),
        [
            "server.status",
            "target.resolve window:w",
            "target.resolve window:x"
        ]
    );
}

#[test]
fn a_read_within_an_allowance_keeps_its_error_and_its_flag() {
    let state = State::new();
    let fixture = fixture("idle-login");
    let rex = rex(
        one_block(fixture.clone()),
        &state,
        Local::from_fixture(&fixture),
    );
    let narrowed = rex.within(Allowance::fixed(Budget {
        timeout: Duration::from_millis(100),
        max_output_bytes: 1024,
    }));
    narrowed.survey(Scope::All, Depth::Spaces).unwrap();
    let unset = AtomicBool::new(false);
    let refusing = rex.within(Allowance::each(
        || Err("the deadline passed".to_owned()),
        &unset,
    ));
    assert_eq!(
        refusing.survey(Scope::All, Depth::Spaces),
        Err(MuxError::Unavailable("the deadline passed".into()))
    );
    let cancelled = AtomicBool::new(true);
    let stopped = rex.within(Allowance::each(|| Ok(Budget::ADAPTER_DEFAULT), &cancelled));
    assert_eq!(
        stopped.survey(Scope::All, Depth::Spaces),
        Err(MuxError::Unavailable("Rex read cancelled".into()))
    );
}

#[test]
fn rex_has_no_integration_root() {
    let state = State::new();
    let rex = rex(
        Script::new(|_, _| Err(rex_error("not_found"))),
        &state,
        Local::default(),
    );
    assert_eq!(rex.integration_root(), Ok(None));
    assert!(rex.wire.calls().is_empty());
}

#[test]
fn a_socket_now_served_by_a_server_with_another_endpoint_is_refused() {
    let state = State::new();
    let fixture = fixture("idle-login");
    let script = Script::new(move |request, events| {
        if request.short() == "server.status" {
            let mut moved = status(PEER);
            moved["listeners"][0]["addresses"][0] = json!("/tmp/other.sock");
            return Ok(moved);
        }
        one_block(fixture.clone()).answer(request, events)
    });
    let rex = rex(script, &state, Local::default());
    assert_eq!(
        rex.survey(Scope::All, Depth::Spaces),
        Err(MuxError::Unavailable(
            "rex:/tmp/r.sock is now served by the Rex server whose endpoint is rex:/tmp/other.sock"
                .into()
        ))
    );
    assert_eq!(rex.wire.calls(), ["server.status"]);
}

/// A tail read can start inside an escape sequence of the line it drops.
#[test]
fn a_screen_read_that_starts_inside_a_link_drops_the_cut_line_before_it_parses() {
    let request = ScreenRequest {
        lines: 60,
        wrap: Wrap::AsDrawn,
        style: Style::Ansi,
    };
    let screen = "\u{1b}]8;;https://example.com/a\u{1b}\\link\u{1b}]8;;\u{1b}\\\r\nsecond\r\n> box";
    assert_eq!(
        super::screen_lines(&screen[10..], 10, request).as_deref(),
        Ok("second\n> box")
    );
}
