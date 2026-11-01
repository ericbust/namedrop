//! Shared wiring and conflict-safe shortcut installation acceptance.
#[path = "support/cleanup.rs"]
mod cleanup;

use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(1);

#[test]
fn observed_claude_context_footer_does_not_replace_task_output() {
    let state = agent_status_ui::State {
        snapshot: Some(serde_json::json!({
            "origin": {"pane_id":"p1", "host":"claude", "label":"lead", "status":"idle"},
            "panes": [{"pane_id":"p1", "host":"claude", "label":"lead", "status":"idle",
                "excerpt":"TASK_RESULT 36 passed, 0 failed\n✻ Worked for 2m 18s · done 9:29 AM\n✔ Update installed · Restart to update\n────────────────────\n❯\n────────────────────\n  worktree-brave-field-6350 git:(worktree-brave-field-6350) Opus 5 ctx:45%\n  ⏵⏵ auto mode on (shift+tab to cycle) · ← 1 agent"}],
            "waits":[], "handoffs_available":true, "warnings":[]
        })),
        ..Default::default()
    };
    let text = agent_status_ui::frame(&state, 84, 24, 0);
    assert!(text.contains("TASK_RESULT 36 passed"), "{text}");
    assert!(!text.contains("ctx:45%"), "{text}");
}

#[allow(dead_code)]
#[path = "../src/agent_status_ui.rs"]
mod agent_status_ui;
#[allow(dead_code)]
#[path = "../src/popup_input.rs"]
mod popup_input;

#[allow(dead_code)]
fn with_runtime_options<T>(
    _options: &namedrop::cli::RuntimeOptions,
    _action: impl FnOnce(
        &namedrop::LifecycleRuntime<
            '_,
            namedrop::ProcessHerdr,
            namedrop::SystemClock,
            namedrop::ProcessIds,
        >,
    ) -> Result<T, String>,
) -> Result<T, String> {
    Err("this suite drives the popup through its injected sources".to_owned())
}

/// Uses real elapsed time to catch both the old popup cutoff and premature cancellation.
#[test]
#[ignore = "takes 21 seconds; run explicitly for integration acceptance"]
fn a_slow_summary_survives_the_former_popup_deadline() {
    use agent_status_ui::{Identity, Screen, Sources, popup};
    use serde_json::json;
    use std::{
        io,
        sync::atomic::AtomicBool,
        thread,
        time::{Duration, Instant},
    };

    let returned = AtomicBool::new(false);
    let rendered = AtomicBool::new(false);
    let cancelled_early = AtomicBool::new(false);
    let collect = |_: &AtomicBool| {
        Ok(json!({
            "origin": {"pane_id":"origin", "label":"lead", "host":"codex", "status":"working"},
            "observed_at_ms": 0,
            "panes":[{"pane_id":"origin", "label":"lead", "host":"codex", "status":"working", "excerpt":"Checking delayed responses"}],
            "waits":[], "handoffs_available":true, "warnings":[], "omitted_panes":0
        }))
    };
    let summarize = |_: &serde_json::Value, cancelled: &AtomicBool| {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(21) {
            if cancelled.load(Ordering::SeqCst) {
                cancelled_early.store(true, Ordering::SeqCst);
                returned.store(true, Ordering::SeqCst);
                return Err("cancelled prematurely".to_owned());
            }
            thread::sleep(Duration::from_millis(10));
        }
        returned.store(true, Ordering::SeqCst);
        Ok(json!({"origin_description":"Delayed valid summary", "peers":[], "summarized_at_ms":0}))
    };
    let recheck = |_: &AtomicBool| Identity::Same;
    struct Keys<'a> {
        rendered: &'a AtomicBool,
        cancelled: &'a AtomicBool,
        started: Instant,
    }
    impl io::Read for Keys<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.rendered.load(Ordering::SeqCst) || self.cancelled.load(Ordering::SeqCst) {
                buffer[0] = b'q';
                return Ok(1);
            }
            if self.started.elapsed() > Duration::from_secs(28) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "summary never rendered",
                ));
            }
            thread::sleep(Duration::from_millis(10));
            Ok(0)
        }
    }
    struct Output<'a> {
        bytes: Vec<u8>,
        rendered: &'a AtomicBool,
    }
    impl io::Write for Output<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.bytes.extend_from_slice(bytes);
            if String::from_utf8_lossy(bytes).contains("Delayed valid summary") {
                self.rendered.store(true, Ordering::SeqCst);
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut screen = Screen {
        input: Keys {
            rendered: &rendered,
            cancelled: &cancelled_early,
            started: Instant::now(),
        },
        output: Output {
            bytes: Vec::new(),
            rendered: &rendered,
        },
        interactive: true,
        width: 72,
        height: 24,
    };
    popup(
        &mut screen,
        &Sources {
            collect: &collect,
            summarize: &summarize,
            recheck: &recheck,
        },
    )
    .unwrap();
    let output = String::from_utf8(screen.output.bytes).unwrap();
    assert!(returned.load(Ordering::SeqCst));
    assert!(
        !cancelled_early.load(Ordering::SeqCst),
        "slow request was cancelled"
    );
    assert!(output.contains("Preparing summary…"));
    assert!(!output.contains("Recent output: Checking delayed responses"));
    assert!(output.contains("Delayed valid summary"));
    assert!(!output.contains("did not finish in time"));
}

struct Fixture(PathBuf);

/// Called only by scripts/verify-agent-status.py --live. Uses ordinary host
/// authentication and reads real panes; this is deliberately absent from CI.
#[test]
#[ignore = "requires explicit live acceptance and authenticated model usage"]
fn live_origin_probe() {
    use namedrop::{
        LifecycleRuntime, ProcessHerdr, ProcessIds, SystemClock, collect_agent_status,
        discover_worktree_for_workspace, summarize_agent_status,
    };
    use serde_json::json;
    use std::{env, sync::atomic::AtomicBool, time::Instant};
    assert_eq!(env::var("NAMEDROP_VERIFY_LIVE").as_deref(), Ok("1"));
    let workspace = env::var("HERDR_WORKSPACE_ID").expect("workspace required");
    let pane_id = env::var("NAMEDROP_VERIFY_PANE").expect("origin pane required");
    let socket = env::var("HERDR_SOCKET_PATH").expect("socket required");
    let state = PathBuf::from(env::var("HERDR_PLUGIN_STATE_DIR").expect("state required"));
    let home = env::var_os("HOME").map(PathBuf::from);
    let cwd = env::current_dir().unwrap();
    let herdr = ProcessHerdr::default().with_socket(Some(socket.clone()));
    let before = discover_worktree_for_workspace(&herdr, &workspace).unwrap();
    let origin = before
        .panes
        .iter()
        .find(|p| p.pane_id == pane_id)
        .expect("origin gone");
    let session = origin.agent_session_id.as_ref().map(|id| id.as_str());
    let clock = SystemClock;
    let ids = ProcessIds::default();
    let runtime = LifecycleRuntime {
        state_directory: &state,
        home_directory: home.as_deref(),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: &cwd,
        workspace_id: Some(&workspace),
        current_pane_id: Some(&pane_id),
        session_socket_id: &socket,
        herdr: &herdr,
        clock: &clock,
        ids: &ids,
    };
    let cancelled = AtomicBool::new(false);
    let started = Instant::now();
    let mut snapshot =
        collect_agent_status(&runtime, &origin.terminal_id, session, &cancelled).unwrap();
    let facts_ms = started.elapsed().as_millis();
    if env::var("NAMEDROP_VERIFY_FACTS_ONLY").as_deref() == Ok("1") {
        assert_eq!(snapshot["handoffs_available"], true, "{snapshot}");
        println!(
            "AGENT_STATUS_FACTS_JSON={}",
            json!({"facts_ms":facts_ms, "origin":snapshot["origin"],
                "handoffs_available":snapshot["handoffs_available"],
                "wait_count":snapshot["waits"].as_array().unwrap().len(),
                "pane_count":snapshot["panes"].as_array().unwrap().len(),
                "warnings":snapshot["warnings"]})
        );
        return; // This mode never invokes a model, including on failure.
    }
    // Exercise live collection and identity, but supply only synthetic task
    // excerpts to the model during acceptance sampling.
    for (index, pane) in snapshot["panes"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        pane["excerpt"] = json!(if index == 0 {
            "Implementing the status popup. Checking tests for refresh and cancellation."
        } else {
            "Reviewing the status snapshot. Checking that changed sessions are rejected."
        });
    }
    for wait in snapshot["waits"].as_array_mut().unwrap() {
        wait["description"] = json!("Review the status popup before integration.");
    }
    let cold_started = Instant::now();
    let cold = summarize_agent_status(&snapshot, &cancelled);
    let cold_ms = cold_started.elapsed().as_millis();
    let mut warm_ms = None;
    let mut cache_reused = false;
    if let Ok(summary) = &cold {
        let warm_started = Instant::now();
        let warm = summarize_agent_status(&snapshot, &cancelled).unwrap();
        warm_ms = Some(warm_started.elapsed().as_millis());
        cache_reused = &warm == summary;
    }
    let after = discover_worktree_for_workspace(&herdr, &workspace).unwrap();
    let unchanged = after.panes.iter().any(|p| {
        p.pane_id == pane_id
            && p.terminal_id == origin.terminal_id
            && p.agent_session_id == origin.agent_session_id
    });
    let observation = json!({
        "pane":pane_id,"host":snapshot["origin"]["host"],"facts_ms":facts_ms,
        "cold_summary_ms":cold_ms,"warm_summary_ms":warm_ms,"cache_reused":cache_reused,
        "origin_unchanged":unchanged,"handoffs_available":snapshot["handoffs_available"],
        "warnings":snapshot["warnings"],
        "model":cold.as_ref().ok().map(|s| s["model"].clone()),
        "summary_error":cold.err(),
        "usage":null
    });
    println!("AGENT_STATUS_PROBE_JSON={observation}");
    assert!(unchanged, "source identity changed during live acceptance");
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "namedrop-status-integration-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn install(&self) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_namedrop"))
            .args(["--json", "ui", "install-shortcuts", "--config"])
            .arg(self.0.join("config.toml"))
            .env("TMPDIR", &self.0)
            .env_remove("HERDR_PLUGIN_STATE_DIR")
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = cleanup::remove_fixture(&self.0);
    }
}

#[test]
fn shortcut_and_package_contract() {
    let fixture = Fixture::new();
    let path = fixture.0.join("config.toml");
    let before = "# keep this comment\n[theme]\nname = 'existing'\n\n[[keys.command]]\nkey = 'prefix+shift+i'\ntype = 'plugin_action'\ncommand = 'namedrop.name-agent'\n";
    fs::write(&path, before).unwrap();
    let output = fixture.install();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let after = fs::read_to_string(&path).unwrap();
    assert!(after.starts_with("# keep this comment\n"));
    let parsed = after.parse::<toml_edit::DocumentMut>().unwrap();
    assert_eq!(parsed["theme"]["name"].as_str(), Some("existing"));
    let commands = parsed["keys"]["command"].as_array_of_tables().unwrap();
    let bound: Vec<_> = commands
        .iter()
        .map(|binding| (binding["key"].as_str(), binding["command"].as_str()))
        .collect();
    // The existing Name binding is kept as it was; only the missing ones are added.
    assert_eq!(
        bound,
        [
            (Some("prefix+shift+i"), Some("namedrop.name-agent")),
            (Some("prefix+shift+s"), Some("namedrop.agent-status")),
            (Some("prefix+shift+m"), Some("namedrop.manage-addresses")),
            (Some("prefix+shift+h"), Some("namedrop.doctor")),
            (Some("prefix+shift+o"), Some("namedrop.tour")),
        ]
    );
    assert!(fixture.install().status.success());
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        after,
        "repeat install must not rewrite config"
    );
    for conflict in [
        "[[keys.command]]\nkey='prefix+shift+s'\ntype='shell'\ncommand='echo keep'\n",
        "[keys.actions]\nsettings='prefix+shift+s'\n",
        "[[keys.command]]\nkey='PREFIX+SHIFT+S'\ntype='shell'\ncommand='echo keep'\n",
    ] {
        fs::write(&path, conflict).unwrap();
        let output = fixture.install();
        assert_eq!(
            output.status.code(),
            Some(1),
            "an occupied key needs attention"
        );
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["data"]["bindings"][0]["status"], "conflict");
        // The occupied key is left alone; the free ones are still added.
        let after = fs::read_to_string(&path).unwrap();
        let parsed = after.parse::<toml_edit::DocumentMut>().unwrap();
        let actions: Vec<_> = parsed["keys"]["command"]
            .as_array_of_tables()
            .unwrap()
            .iter()
            .filter_map(|binding| binding["command"].as_str().map(str::to_owned))
            .filter(|command| command.starts_with("namedrop."))
            .collect();
        assert_eq!(
            actions,
            [
                "namedrop.name-agent",
                "namedrop.manage-addresses",
                "namedrop.doctor",
                "namedrop.tour"
            ]
        );
        let original = conflict.parse::<toml_edit::DocumentMut>().unwrap();
        assert_eq!(
            parsed["keys"].get("actions").map(ToString::to_string),
            original["keys"].get("actions").map(ToString::to_string)
        );
    }
    let manifest = include_str!("../herdr-plugin.toml")
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    for (kind, args) in [
        (
            "actions",
            vec!["sh", "./scripts/run.sh", "ui", "launch", "status"],
        ),
        (
            "panes",
            vec!["sh", "./scripts/run.sh", "ui", "run", "status"],
        ),
    ] {
        let entry = manifest[kind]
            .as_array_of_tables()
            .unwrap()
            .iter()
            .find(|entry| entry["id"].as_str() == Some("agent-status"))
            .unwrap();
        let actual = entry["command"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(actual, args);
        if kind == "panes" {
            assert_eq!(entry["placement"].as_str(), Some("popup"));
        }
    }
}

/// Explicit acceptance only; never reads a live pane or sends real task text.
#[test]
#[ignore = "requires approval for four bounded synthetic provider calls"]
fn synthetic_worker_reliability_probe() {
    use namedrop::summarize_agent_status;
    use serde_json::json;
    use std::{env, sync::atomic::AtomicBool, time::Instant};
    assert_eq!(env::var("NAMEDROP_VERIFY_LIVE").as_deref(), Ok("1"));
    let host = env::var("NAMEDROP_VERIFY_HOST").unwrap();
    assert!(matches!(host.as_str(), "claude" | "codex"));
    let dense = env::var("NAMEDROP_VERIFY_DENSE").as_deref() == Ok("1");
    let mut panes = Vec::new();
    for index in 0..4 {
        let excerpt = if dense {
            (0..100).map(|line| format!("Synthetic task {index}, step {line}: checking popup refresh, cancellation, and safe text rendering.\n")).collect::<String>()
        } else {
            format!(
                "Synthetic task {index}: checking popup refresh and cancellation. Local test passed."
            )
        };
        panes.push(json!({"pane_id":format!("synthetic-{index}"), "terminal_id":format!("terminal-{index}"),
            "agent_session_id":format!("session-{index}"),"label":format!("test-agent-{index}"),"host":host,
            "status":"working","excerpt":excerpt}));
    }
    let snapshot = json!({"origin":{"pane_id":"synthetic-0","terminal_id":"terminal-0",
        "agent_session_id":"session-0","workspace_id":"synthetic-workspace","host":host,"label":"test-agent-0"},
        "panes":panes,"waits":[],"handoffs_available":true,"warnings":[],"omitted_panes":0,"observed_at_ms":0});
    let cancel = AtomicBool::new(false);
    let start = Instant::now();
    let result = summarize_agent_status(&snapshot, &cancel);
    let elapsed_ms = start.elapsed().as_millis();
    let mut warm_ms = None;
    let mut reused = false;
    if let Ok(summary) = &result {
        let start = Instant::now();
        let warm = summarize_agent_status(&snapshot, &cancel).unwrap();
        warm_ms = Some(start.elapsed().as_millis());
        reused = warm == *summary;
    }
    println!(
        "RELIABILITY_PROBE_JSON={}",
        json!({"host":host,"dense":dense,"snapshot_bytes":snapshot.to_string().len(),
        "elapsed_ms":elapsed_ms,"warm_ms":warm_ms,"cache_reused":reused,"result":result})
    );
}
