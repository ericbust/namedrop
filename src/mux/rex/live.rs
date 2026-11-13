use super::{
    control::{Connection, Control, Fault, Limits},
    local::{peer_pid, prove_local, tests::forward},
    ws::{Message, WebSocket},
};
use crate::mux::{Budget, MuxKind};
use serde_json::json;
use std::{os::unix::net::UnixStream, path::PathBuf, sync::atomic::AtomicBool};

fn socket() -> PathBuf {
    let url = std::env::var("NAMEDROP_REX_LIVE")
        .expect("set NAMEDROP_REX_LIVE to a scratch Rex server's unix:// URL");
    PathBuf::from(
        url.strip_prefix("unix://")
            .expect("NAMEDROP_REX_LIVE is a unix:// URL"),
    )
}

#[test]
#[ignore = "run with NAMEDROP_REX_LIVE=unix://<scratch server socket> and -- --ignored --test-threads=1"]
fn ws_reads_the_welcome_of_a_real_server() {
    let stream = UnixStream::connect(socket()).unwrap();
    let mut socket = WebSocket::connect(stream, "/api/control", "rex.control.v1", 1 << 20).unwrap();
    socket.upgrade().unwrap();
    socket
        .send_text(r#"{"type":"hello","api_revision":12,"client":{"kind":"other","name":"namedrop-live"}}"#)
        .unwrap();
    let Message::Text(welcome) = socket.recv().unwrap() else {
        panic!("the welcome is a text message");
    };
    let welcome: serde_json::Value = serde_json::from_str(&welcome).unwrap();
    assert_eq!(welcome["type"], "welcome", "{welcome}");
    assert_eq!(welcome["protocol"], "rex.control.v1", "{welcome}");
    assert_eq!(welcome["api_revision"], 12, "{welcome}");
    socket.close(1000).unwrap();
}

#[test]
#[ignore = "run with NAMEDROP_REX_LIVE=unix://<scratch server socket> and -- --ignored --test-threads=1"]
fn control_reads_the_server_status_and_a_typed_error() {
    let cancelled = AtomicBool::new(false);
    let budget = || Ok(Budget::ADAPTER_DEFAULT);
    let mut connection = Connection::open(
        UnixStream::connect(socket()).unwrap(),
        Limits {
            budget: Box::new(budget),
            cancelled: &cancelled,
        },
    )
    .unwrap();
    let status = connection.request("server.status", None).unwrap();
    assert_eq!(status.seq, 0, "no events before the first request");
    assert!(
        status.payload["pid"].as_u64().is_some(),
        "{}",
        status.payload
    );
    let missing = json!({"session_id": "session:00000000-0000-0000-0000-000000000000"});
    match connection.request("session.attach", Some(missing)) {
        Err(Fault::Rex { code, status, .. }) => {
            assert_eq!((code.as_str(), status), ("session_not_found", 404));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
#[ignore = "run with NAMEDROP_REX_LIVE=unix://<scratch server socket> and -- --ignored --test-threads=1"]
fn the_scratch_server_is_local_and_a_socket_forwarded_to_it_is_not() {
    let cancelled = AtomicBool::new(false);
    let budget = || Ok(Budget::ADAPTER_DEFAULT);
    let prove = |dial: &std::path::Path| {
        let stream = UnixStream::connect(dial).unwrap();
        let peer = peer_pid(&stream).unwrap();
        let limits = Limits {
            budget: Box::new(budget),
            cancelled: &cancelled,
        };
        let mut control = Connection::open(stream, limits).unwrap();
        (peer, prove_local(dial, &mut control, peer))
    };
    let (peer, server) = prove(&socket());
    let server = server.unwrap();
    assert_eq!(server.pid, peer);
    assert_eq!(server.endpoint.kind, MuxKind::Rex);
    assert_eq!(server.endpoint.unix_socket(), Some(socket().as_path()));
    assert!(server.host_id.is_some());

    let forwarded = std::env::temp_dir().join(format!("nd-live-fwd-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&forwarded);
    let forwarder = forward(&forwarded, socket());
    let (peer, refused) = prove(&forwarded);
    assert_eq!(peer, std::process::id());
    match refused {
        Err(Fault::NotLocal(why)) => assert!(
            why.contains(&format!("the server is process {}", server.pid)),
            "{why}"
        ),
        other => panic!(
            "a forwarded socket was proved local: {:?}",
            other.map(|server| server.endpoint)
        ),
    }
    forwarder.join().unwrap();
    std::fs::remove_file(&forwarded).unwrap();
}

fn live_rex() -> super::RexMux<super::Socket> {
    let socket = socket();
    let state = std::env::temp_dir().join(format!("nd-rex-live-{}", std::process::id()));
    super::RexMux::new(
        super::Socket {
            path: socket.clone(),
            budget: Budget::ADAPTER_DEFAULT,
            cancelled: std::sync::Arc::default(),
        },
        crate::mux::Endpoint::parse(&format!("rex:{}", socket.display())),
        &state,
        std::sync::Arc::new(crate::mux::processes::ThisMachine),
    )
}

/// The panes of the session labelled `label`.
fn panes(rex: &dyn crate::mux::MuxRead, label: &str) -> Vec<crate::mux::PaneFacts> {
    use crate::mux::{Depth, Scope};
    let spaces = rex.survey(Scope::All, Depth::Spaces).unwrap().spaces;
    let space = spaces
        .iter()
        .find(|space| space.label.as_deref() == Some(label))
        .unwrap_or_else(|| panic!("no session labelled {label}"));
    let survey = rex.survey(Scope::PanesOf(&space.id), Depth::Panes).unwrap();
    survey.spaces.into_iter().next().unwrap().panes
}

/// Seconds since the epoch at which `pid` started, as `ps` shows it.
fn ps_started(pid: u32) -> u64 {
    let lstart = std::process::Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let lstart = String::from_utf8(lstart.stdout).unwrap();
    let seconds = std::process::Command::new("date")
        .args(["-j", "-f", "%a %b %d %T %Y", lstart.trim(), "+%s"])
        .output()
        .unwrap();
    String::from_utf8(seconds.stdout)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// The scratch server holds sessions labelled `nd-live-agent` (a program
/// that reports `claude-code` with OSC 7501), `nd-live-shell` (zsh at its
/// prompt), and `nd-live-python` (`-- python3 -i`).
#[test]
#[ignore = "run with NAMEDROP_REX_LIVE=unix://<scratch server socket> and -- --ignored --test-threads=1"]
fn a_survey_of_a_scratch_server_shows_the_agent_and_which_blocks_have_a_shell() {
    use crate::mux::{AgentKind, MuxRead, SessionClaim};
    let rex = live_rex();
    let agent = panes(&rex, "nd-live-agent").remove(0);
    let identity = agent.agent.expect("the reporting program is an agent");
    assert_eq!(
        identity.kind,
        AgentKind::Known(crate::AgentRuntime::ClaudeCode)
    );
    let SessionClaim::Unreported { incarnation } = identity.session else {
        panic!("an unbound agent is unreported: {identity:?}");
    };
    let (pid, start) = incarnation.0.split_once('@').unwrap();
    let start_seconds = start.parse::<u64>().unwrap() / 1_000_000;
    assert_eq!(
        start_seconds,
        ps_started(pid.parse().unwrap()),
        "{incarnation:?}"
    );

    let shell = panes(&rex, "nd-live-shell").remove(0);
    assert!(
        crate::terminal::shell_is_available(&rex, shell.id.as_str()),
        "{:?}",
        rex.process(&shell.id)
    );
    let python = panes(&rex, "nd-live-python").remove(0);
    assert_eq!(rex.process(&python.id).unwrap().shell_pid, None);
}

fn block_from(variable: &str) -> crate::mux::PaneId {
    crate::mux::PaneId::new(
        std::env::var(variable).unwrap_or_else(|_| panic!("set {variable} to a block ID")),
    )
}

fn wait_for(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    while !done() {
        assert!(
            std::time::Instant::now() < deadline,
            "{what} did not happen"
        );
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

/// `NAMEDROP_REX_LIVE_SHELL` names a block with zsh at its prompt.
#[test]
#[ignore = "run with NAMEDROP_REX_LIVE, NAMEDROP_REX_LIVE_SHELL, and -- --ignored --test-threads=1"]
fn a_line_typed_into_a_scratch_shell_runs_there() {
    use crate::mux::InputKind;
    let rex = live_rex();
    let shell = block_from("NAMEDROP_REX_LIVE_SHELL");
    let marker = std::env::temp_dir().join(format!("nd-rex-live-line-{}", std::process::id()));
    let _ = std::fs::remove_file(&marker);
    let line = format!("echo typed-by-namedrop > {}", marker.display());
    assert_eq!(
        rex.send(&shell, InputKind::Line(&line)),
        crate::mux::InputOutcome::Delivered
    );
    wait_for("the line", || marker.exists());
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap(),
        "typed-by-namedrop\n"
    );
    std::fs::remove_file(&marker).unwrap();
}

/// `NAMEDROP_REX_LIVE_CLAUDE` names a block running Claude Code, idle at its
/// prompt. No hook binds it before commit 17, so the test writes the binding
/// a hook would write.
#[test]
#[ignore = "run with NAMEDROP_REX_LIVE, NAMEDROP_REX_LIVE_CLAUDE, and -- --ignored --test-threads=1"]
fn a_prompt_reaches_a_scratch_claude_code_through_the_gate() {
    use crate::mux::{
        Depth, InputKind, InputOutcome, MuxRead, PromptRequest, Scope, ScreenRequest, SessionClaim,
        Style, Wrap,
    };
    let rex = live_rex();
    let block = block_from("NAMEDROP_REX_LIVE_CLAUDE");
    let pane = |rex: &super::RexMux<super::Socket>| {
        let spaces = rex.survey(Scope::All, Depth::Panes).unwrap().spaces;
        spaces
            .into_iter()
            .flat_map(|space| space.panes)
            .find(|pane| pane.id == block)
            .expect("the survey lists the block")
    };
    let found = pane(&rex);
    let agent = found.agent.expect("Claude Code reports itself");
    let SessionClaim::Unreported { incarnation } = agent.session else {
        panic!("an unbound agent is unreported: {agent:?}");
    };
    let binding = super::binding::Binding {
        block: block.as_str().to_owned(),
        endpoint: rex.endpoint.key.clone(),
        space: found.space.as_str().to_owned(),
        foreground: incarnation.0.clone(),
        agent: incarnation.0,
        runtime: "claude-code".into(),
        session: "live-session".into(),
        bound_at_ms: 0,
    };
    let path = rex.bindings.path(block.as_str()).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec(&binding).unwrap()).unwrap();
    let expect = pane(&rex).agent.expect("the bound agent");
    assert!(
        matches!(&expect.session, SessionClaim::Reported(session) if session.as_str() == "live-session")
    );
    let outcome = rex.send(
        &block,
        InputKind::Prompt(PromptRequest {
            expect: &expect,
            text: "Reply with exactly the word pong.\nNothing else.",
        }),
    );
    assert_eq!(outcome, InputOutcome::Delivered);
    let screen = ScreenRequest {
        lines: 40,
        wrap: Wrap::AsDrawn,
        style: Style::Plain,
    };
    wait_for("Claude Code's answer", || {
        rex.screen(&block, screen).is_ok_and(|screen| {
            screen
                .text
                .lines()
                .any(|line| line.contains("pong") && !line.contains("word pong"))
        })
    });
    std::fs::remove_file(path).unwrap();
}
