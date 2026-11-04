use namedrop::AgentRuntime;
use namedrop::subagent::{
    DiskFiles, HOST_FILE_CAP, HostFiles, SubagentEdge, SubagentSighting, SubagentText,
};
use serde_json::Value;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

fn fixture(name: &str) -> Value {
    let all: Value =
        serde_json::from_str(include_str!("fixtures/subagent_hooks.json")).expect("fixture JSON");
    all[name].clone()
}

#[derive(Default)]
struct FakeFiles(HashMap<PathBuf, String>);

impl FakeFiles {
    fn with(mut self, path: impl Into<PathBuf>, content: impl Into<String>) -> Self {
        self.0.insert(path.into(), content.into());
        self
    }
}

impl HostFiles for FakeFiles {
    fn read_head(&self, path: &Path, cap: usize, first_line_only: bool) -> Option<String> {
        let text = self.0.get(path)?;
        let text = if first_line_only {
            text.lines().next().unwrap_or_default()
        } else {
            text.as_str()
        };
        (text.len() <= cap).then(|| text.to_owned())
    }
}

fn parse(host: AgentRuntime, payload: &Value, files: &FakeFiles) -> Option<SubagentSighting> {
    SubagentSighting::parse(host, &payload.to_string(), files)
}

fn text(value: &Option<SubagentText>) -> Option<&str> {
    value.as_ref().map(SubagentText::as_str)
}

fn claude_meta_path(stop: &Value) -> PathBuf {
    let transcript = PathBuf::from(stop["agent_transcript_path"].as_str().unwrap());
    let id = stop["agent_id"].as_str().unwrap();
    transcript
        .parent()
        .unwrap()
        .join(format!("agent-{id}.meta.json"))
}

fn codex_rollout_line() -> String {
    fixture("codex_child_rollout_first_line").to_string()
}

#[test]
fn claude_start_names_the_type_and_never_reads_meta_that_appears_later() {
    let start = fixture("claude_start_foreground");
    let stop = fixture("claude_stop_foreground");
    let files = FakeFiles::default().with(
        claude_meta_path(&stop),
        fixture("claude_meta_foreground").to_string(),
    );

    let sighting = parse(AgentRuntime::ClaudeCode, &start, &files).expect("start parses");

    assert_eq!(sighting.edge, SubagentEdge::Start);
    assert_eq!(sighting.id.as_str(), "a9a6420ee6bb17834");
    assert_eq!(
        sighting.parent_session.as_ref().map(|s| s.as_str()),
        Some("04860049-89c4-4f88-9c07-e1adce50b758")
    );
    assert_eq!(text(&sighting.kind), Some("Explore"));
    assert_eq!(text(&sighting.name), None);
}

#[test]
fn claude_stop_reads_the_description_from_the_sibling_meta_file() {
    let stop = fixture("claude_stop_foreground");
    let files = FakeFiles::default().with(
        claude_meta_path(&stop),
        fixture("claude_meta_foreground").to_string(),
    );

    let sighting = parse(AgentRuntime::ClaudeCode, &stop, &files).expect("stop parses");

    assert_eq!(sighting.edge, SubagentEdge::Stop);
    assert_eq!(text(&sighting.kind), Some("Explore"));
    assert_eq!(text(&sighting.name), Some("list files"));
}

#[test]
fn claude_stop_without_meta_keeps_the_type_and_has_no_name() {
    let stop = fixture("claude_stop_foreground");

    let sighting = parse(AgentRuntime::ClaudeCode, &stop, &FakeFiles::default()).unwrap();

    assert_eq!(text(&sighting.kind), Some("Explore"));
    assert_eq!(text(&sighting.name), None);
}

#[test]
fn claude_background_stop_takes_the_name_from_its_own_background_task() {
    let stop = fixture("claude_stop_background");

    let sighting = parse(AgentRuntime::ClaudeCode, &stop, &FakeFiles::default()).unwrap();

    assert_eq!(text(&sighting.kind), Some("general-purpose"));
    assert_eq!(text(&sighting.name), Some("say hi"));
}

#[test]
fn claude_stop_ignores_a_background_task_of_another_subagent() {
    let mut stop = fixture("claude_stop_background");
    stop["background_tasks"][0]["id"] = "someone-else".into();

    let sighting = parse(AgentRuntime::ClaudeCode, &stop, &FakeFiles::default()).unwrap();

    assert_eq!(text(&sighting.name), None);
}

#[test]
fn claude_stop_refuses_a_transcript_path_that_is_not_this_subagents_own() {
    let mut stop = fixture("claude_stop_foreground");
    let meta = claude_meta_path(&stop);
    stop["agent_transcript_path"] = "/elsewhere/agent-a9a6420ee6bb17834.jsonl".into();
    let files = FakeFiles::default()
        .with(meta, fixture("claude_meta_foreground").to_string())
        .with(
            "/elsewhere/agent-a9a6420ee6bb17834.meta.json",
            fixture("claude_meta_foreground").to_string(),
        );

    let sighting = parse(AgentRuntime::ClaudeCode, &stop, &files).unwrap();

    assert_eq!(text(&sighting.name), None);
}

#[test]
fn codex_start_names_the_task_and_nickname_from_the_child_rollout() {
    let start = fixture("codex_start");
    let files = FakeFiles::default().with(
        start["transcript_path"].as_str().unwrap(),
        format!("{}\n{{\"type\":\"turn\"}}\n", codex_rollout_line()),
    );

    let sighting = parse(AgentRuntime::Codex, &start, &files).expect("start parses");

    assert_eq!(sighting.edge, SubagentEdge::Start);
    assert_eq!(sighting.id.as_str(), "01a1466d-e42f-7511-b2b8-d48fcb7f158b");
    assert_eq!(text(&sighting.name), Some("say_hi (Aristotle)"));
    assert_eq!(text(&sighting.kind), None, "\"default\" is not a type");
}

#[test]
fn codex_stop_reads_the_agent_transcript_not_the_parent_one() {
    let stop = fixture("codex_stop");
    let files = FakeFiles::default()
        .with(
            stop["transcript_path"].as_str().unwrap(),
            "{\"type\":\"session_meta\",\"payload\":{}}",
        )
        .with(
            stop["agent_transcript_path"].as_str().unwrap(),
            codex_rollout_line(),
        );

    let sighting = parse(AgentRuntime::Codex, &stop, &files).unwrap();

    assert_eq!(sighting.edge, SubagentEdge::Stop);
    assert_eq!(text(&sighting.name), Some("say_hi (Aristotle)"));
}

#[test]
fn codex_named_agent_type_is_kept() {
    let mut start = fixture("codex_start");
    start["agent_type"] = "explorer".into();

    let sighting = parse(AgentRuntime::Codex, &start, &FakeFiles::default()).unwrap();

    assert_eq!(text(&sighting.kind), Some("explorer"));
    assert_eq!(text(&sighting.name), None);
}

#[test]
fn invalid_input_is_ignored() {
    let start = fixture("claude_start_foreground");
    let files = FakeFiles::default();
    for (label, payload) in [
        ("traversal id", {
            let mut p = start.clone();
            p["agent_id"] = "../x".into();
            p
        }),
        ("missing id", {
            let mut p = start.clone();
            p.as_object_mut().unwrap().remove("agent_id");
            p
        }),
        ("other event", {
            let mut p = start.clone();
            p["hook_event_name"] = "PreToolUse".into();
            p
        }),
    ] {
        assert_eq!(
            parse(AgentRuntime::ClaudeCode, &payload, &files),
            None,
            "{label}"
        );
    }
    assert_eq!(
        SubagentSighting::parse(AgentRuntime::Codex, "not json", &files),
        None
    );
}

#[test]
fn an_unparseable_rollout_line_gives_no_name() {
    let start = fixture("codex_start");
    let path = start["transcript_path"].as_str().unwrap();
    for line in [
        "not json",
        "{\"type\":\"turn\"}",
        "{\"type\":\"session_meta\",\"payload\":{}}",
    ] {
        let files = FakeFiles::default().with(path, line);
        let sighting = parse(AgentRuntime::Codex, &start, &files).unwrap();
        assert_eq!(text(&sighting.name), None, "{line}");
    }
}

#[test]
fn subagent_text_strips_control_and_invisible_characters_and_caps_length() {
    assert_eq!(
        SubagentText::parse("  list\u{202E}\tfiles\n ").map(|t| t.as_str().to_owned()),
        Some("list files".to_owned())
    );
    assert_eq!(SubagentText::parse("\u{200B} \n"), None);
    assert_eq!(
        SubagentText::parse(&"x".repeat(500)).map(|t| t.as_str().chars().count()),
        Some(80)
    );
}

#[test]
fn disk_files_read_bounded_regular_files_only() {
    let directory = std::env::temp_dir().join(format!("namedrop-subagent-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let file = directory.join("rollout.jsonl");
    std::fs::write(&file, "first\nsecond\n").unwrap();
    let link = directory.join("link.jsonl");
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&file, &link).unwrap();
    let big = directory.join("big.json");
    std::fs::write(&big, "y".repeat(HOST_FILE_CAP + 1)).unwrap();

    assert_eq!(
        DiskFiles.read_head(&file, HOST_FILE_CAP, true).as_deref(),
        Some("first")
    );
    assert_eq!(
        DiskFiles.read_head(&file, HOST_FILE_CAP, false).as_deref(),
        Some("first\nsecond\n")
    );
    assert_eq!(
        DiskFiles.read_head(&link, HOST_FILE_CAP, false),
        None,
        "symlink"
    );
    assert_eq!(
        DiskFiles.read_head(&directory, HOST_FILE_CAP, false),
        None,
        "directory"
    );
    assert_eq!(
        DiskFiles.read_head(&big, HOST_FILE_CAP, false),
        None,
        "over the cap"
    );
    assert_eq!(
        DiskFiles.read_head(&directory.join("missing"), HOST_FILE_CAP, false),
        None
    );
    std::fs::remove_dir_all(&directory).unwrap();
}

use namedrop::subagent::{Event, MAX_RUN_AGE_MS, ParentIdentity, record, revision, running};
use serde_json::json;
use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

struct State(PathBuf);
impl State {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "namedrop-subagent-state-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("home")).unwrap();
        fs::create_dir_all(root.join("xdg")).unwrap();
        Self(root)
    }
    fn parent(&self) -> ParentIdentity {
        ParentIdentity::new("test-socket", "test-workspace", "pane-a", "parent-session").unwrap()
    }
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_namedrop"));
        command
            .env_clear()
            .env("HOME", self.0.join("home"))
            .env("XDG_CONFIG_HOME", self.0.join("xdg"))
            .env("HERDR_SOCKET_PATH", "test-socket")
            .env("HERDR_WORKSPACE_ID", "test-workspace")
            .env("HERDR_PANE_ID", "pane-a")
            .args([
                "hook",
                "subagent",
                "--runtime",
                "claude-code",
                "--state-directory",
            ])
            .arg(&self.0);
        command
    }
}
impl Drop for State {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn start(id: &str) -> Event {
    Event::Start {
        generation: None,
        id: id.into(),
        kind: Some("Explore".into()),
        description: Some("read sources".into()),
    }
}
fn ids(state: &State, now: i64) -> Vec<String> {
    running(&state.0, &state.parent(), now, None)
        .into_iter()
        .map(|r| r.id)
        .collect()
}

#[test]
fn odd_background_shapes_do_not_drop_a_stop() {
    for tasks in [json!({"id": 7}), json!([{"id":7}, "x"]), json!(null)] {
        let mut stop = fixture("claude_stop_background");
        stop["background_tasks"] = tasks.clone();
        assert_eq!(
            parse(AgentRuntime::ClaudeCode, &stop, &FakeFiles::default())
                .unwrap()
                .edge,
            SubagentEdge::Stop
        );
        stop["hook_event_name"] = json!("Stop");
        assert!(
            matches!(Event::parse(AgentRuntime::ClaudeCode, &stop.to_string(), &FakeFiles::default()).unwrap().1, Event::ParentStop { background, .. } if background.is_empty())
        );
    }
}

#[test]
fn concurrent_siblings_and_stop_before_start_keep_exact_running_set() {
    let state = State::new();
    let initial = revision(&state.0);
    std::thread::scope(|scope| {
        for id in 0..24 {
            let state = &state;
            scope.spawn(move || {
                record(
                    &state.0,
                    &state.parent(),
                    start(&format!("child-{id:02}")),
                    100,
                )
                .unwrap();
            });
        }
    });
    assert_eq!(ids(&state, 200).len(), 24);
    assert_ne!(initial, revision(&state.0));
    record(
        &state.0,
        &state.parent(),
        Event::Stop {
            generation: None,
            id: "late".into(),
        },
        201,
    )
    .unwrap();
    record(&state.0, &state.parent(), start("late"), 202).unwrap();
    assert_eq!(ids(&state, 203).len(), 24);
    let other =
        ParentIdentity::new("test-socket", "test-workspace", "pane-a", "other-session").unwrap();
    assert!(running(&state.0, &other, 203, None).is_empty());
}

#[test]
fn host_turn_identity_allows_same_agent_on_a_new_turn_and_keeps_old_tombstone() {
    let state = State::new();
    for (edge, turn, at) in [
        ("SubagentStop", "turn-1", 100),
        ("SubagentStart", "turn-1", 101),
        ("SubagentStart", "turn-2", 102),
    ] {
        let payload = json!({"session_id":"parent-session", "agent_id":"resumed", "agent_type":"Explore", "hook_event_name":edge, "prompt_id":turn});
        let (_, event) = Event::parse(
            AgentRuntime::ClaudeCode,
            &payload.to_string(),
            &FakeFiles::default(),
        )
        .unwrap();
        record(&state.0, &state.parent(), event, at).unwrap();
    }
    let runs = running(&state.0, &state.parent(), 103, None);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].started_at_ms, 102);
    let malformed = json!({"session_id":"parent-session", "agent_id":"resumed", "hook_event_name":"SubagentStop", "agent_type":[7], "agent_transcript_path":{}, "background_tasks":17});
    let (_, event) = Event::parse(
        AgentRuntime::ClaudeCode,
        &malformed.to_string(),
        &FakeFiles::default(),
    )
    .unwrap();
    record(&state.0, &state.parent(), event, 104).unwrap();
    assert!(
        ids(&state, 105).is_empty(),
        "stop with unknown generation is conservative"
    );
}

fn record_host_event(state: &State, payload: Value, at: i64) {
    let (_, event) = Event::parse(
        AgentRuntime::ClaudeCode,
        &payload.to_string(),
        &FakeFiles::default(),
    )
    .unwrap();
    record(&state.0, &state.parent(), event, at).unwrap();
}

fn host_event(edge: &str, generation: &str, id: &str) -> Value {
    json!({"session_id":"parent-session", "hook_event_name":edge,
        "prompt_id":generation, "agent_id":id, "agent_type":"Explore"})
}

#[test]
fn ended_session_requires_a_new_proven_prompt_before_showing_new_work() {
    for (end_generation, source) in [
        (Some("turn-1"), "resume"),
        (None, "resume"),
        (None, "compact"),
    ] {
        let state = State::new();
        record_host_event(&state, host_event("UserPromptSubmit", "turn-1", ""), 100);
        record_host_event(&state, host_event("SubagentStart", "turn-1", "old"), 110);
        let mut end = host_event("SessionEnd", "turn-1", "");
        end["prompt_id"] = json!(end_generation);
        record_host_event(&state, end, 120);
        let resumed = json!({"session_id":"parent-session", "hook_event_name":"SessionStart", "source":source, "prompt_id":"turn-2"});
        record_host_event(&state, resumed, 130);
        record_host_event(&state, host_event("SubagentStart", "turn-2", "new"), 140);
        assert!(
            ids(&state, 145).is_empty(),
            "SessionStart alone cannot prove a new prompt"
        );
        record_host_event(&state, host_event("UserPromptSubmit", "turn-2", ""), 150);
        record_host_event(
            &state,
            host_event("SubagentStart", "turn-1", "delayed-old"),
            160,
        );
        assert_eq!(ids(&state, 170), ["new"], "{end_generation:?} {source}");
    }
}

#[test]
fn delayed_parent_stop_does_not_hide_a_newer_typed_turn() {
    let state = State::new();
    record_host_event(&state, host_event("UserPromptSubmit", "turn-1", ""), 100);
    record_host_event(&state, host_event("SubagentStart", "turn-1", "old"), 110);
    record_host_event(&state, host_event("UserPromptSubmit", "turn-2", ""), 120);
    record_host_event(&state, host_event("SubagentStart", "turn-2", "new"), 130);
    record_host_event(&state, host_event("Stop", "turn-1", ""), 140);
    record_host_event(
        &state,
        host_event("SubagentStart", "turn-1", "late-old"),
        150,
    );
    assert_eq!(ids(&state, 160), ["new"]);
}

#[test]
fn background_exemption_belongs_to_the_child_run_generation() {
    let state = State::new();
    record_host_event(&state, host_event("SubagentStart", "turn-1", "child"), 100);
    let mut stop = host_event("Stop", "turn-1", "");
    stop["background_tasks"] = json!([{"id":"child", "status":"running"}]);
    record_host_event(&state, stop, 110);
    record_host_event(&state, host_event("SubagentStop", "turn-1", "child"), 120);
    record_host_event(&state, host_event("UserPromptSubmit", "turn-2", ""), 130);
    record_host_event(&state, host_event("SubagentStart", "turn-2", "child"), 140);
    assert_eq!(ids(&state, 145), ["child"]);
    assert!(running(&state.0, &state.parent(), 160, Some(150)).is_empty());
    assert!(
        ids(&state, 170).is_empty(),
        "native idle cleanup stays durable"
    );
}

#[test]
fn delayed_typed_end_and_reset_do_not_hide_new_proven_prompt() {
    for edge in ["SessionEnd", "SessionStart"] {
        let state = State::new();
        record_host_event(&state, host_event("UserPromptSubmit", "turn-1", ""), 100);
        record_host_event(&state, host_event("SubagentStart", "turn-1", "old"), 110);
        record_host_event(&state, host_event("UserPromptSubmit", "turn-2", ""), 120);
        record_host_event(&state, host_event("SubagentStart", "turn-2", "new"), 130);
        record_host_event(&state, host_event(edge, "turn-1", ""), 140);
        record_host_event(
            &state,
            host_event("SubagentStart", "turn-1", "late-old"),
            150,
        );
        assert_eq!(ids(&state, 160), ["new"], "{edge}");
    }
}

#[test]
fn original_typed_background_run_survives_new_parent_prompts_and_stops() {
    let state = State::new();
    record_host_event(
        &state,
        host_event("SubagentStart", "turn-1", "background"),
        100,
    );
    let mut stop = host_event("Stop", "turn-1", "");
    stop["background_tasks"] = json!([{"id":"background", "status":"running"}]);
    record_host_event(&state, stop, 110);
    record_host_event(&state, host_event("UserPromptSubmit", "turn-2", ""), 120);
    record_host_event(
        &state,
        host_event("SubagentStart", "turn-2", "foreground"),
        130,
    );
    record_host_event(&state, host_event("Stop", "turn-2", ""), 140);
    let runs = running(&state.0, &state.parent(), 160, Some(150));
    assert_eq!(
        runs.iter().map(|run| run.id.as_str()).collect::<Vec<_>>(),
        ["background"]
    );
}

#[test]
fn malformed_fact_or_reader_overflow_hides_all_runs() {
    let state = State::new();
    record(&state.0, &state.parent(), start("one"), 100).unwrap();
    let directory = fs::read_dir(state.0.join("subagents/v1"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let broken = directory.join("broken.stop.json");
    fs::write(&broken, "interrupted or corrupt").unwrap();
    assert!(ids(&state, 101).is_empty());
    fs::remove_file(broken).unwrap();
    for id in 0..4096 {
        fs::write(directory.join(format!("{id}.tmp")), []).unwrap();
    }
    assert!(ids(&state, 101).is_empty());
}

#[test]
fn parent_stop_preserves_explicit_background_across_turns_and_idle_is_durable() {
    let state = State::new();
    for id in ["foreground", "background"] {
        record(&state.0, &state.parent(), start(id), 100).unwrap();
    }
    let payload = json!({"session_id":"parent-session", "hook_event_name":"Stop", "background_tasks":[{"id":"background","status":"running"}, {"id":"foreground","status":"completed"}]});
    let (_, event) = Event::parse(
        AgentRuntime::ClaudeCode,
        &payload.to_string(),
        &FakeFiles::default(),
    )
    .unwrap();
    record(&state.0, &state.parent(), event, 200).unwrap();
    assert_eq!(ids(&state, 201), ["background"]);
    assert_eq!(running(&state.0, &state.parent(), 202, Some(201)).len(), 1);
    record(&state.0, &state.parent(), Event::Resume, 300).unwrap();
    assert_eq!(ids(&state, 301), ["background"]);
    record(&state.0, &state.parent(), start("new-turn"), 302).unwrap();
    assert_eq!(ids(&state, 303), ["background", "new-turn"]);
    assert_eq!(running(&state.0, &state.parent(), 305, Some(304)).len(), 1);
    assert_eq!(
        ids(&state, 306),
        ["background"],
        "idle cleanup cannot revive on next turn"
    );
    record(
        &state.0,
        &state.parent(),
        Event::Stop {
            generation: None,
            id: "background".into(),
        },
        307,
    )
    .unwrap();
    assert!(ids(&state, 308).is_empty());
}

#[test]
fn reset_session_end_and_age_bound_remove_stale_runs() {
    let state = State::new();
    record(&state.0, &state.parent(), start("old"), 100).unwrap();
    assert_eq!(ids(&state, MAX_RUN_AGE_MS + 99), ["old"]);
    record(&state.0, &state.parent(), start("old"), MAX_RUN_AGE_MS + 99).unwrap();
    assert!(ids(&state, MAX_RUN_AGE_MS + 100).is_empty());
    record(&state.0, &state.parent(), Event::Reset, 200).unwrap();
    record(&state.0, &state.parent(), start("new"), 300).unwrap();
    assert_eq!(ids(&state, 301), ["new"]);
    record(&state.0, &state.parent(), Event::ParentEnd, 400).unwrap();
    record(&state.0, &state.parent(), start("late"), 401).unwrap();
    record(&state.0, &state.parent(), Event::Resume, 402).unwrap();
    assert!(ids(&state, 403).is_empty());
}

#[test]
fn installed_command_is_silent_and_file_only_even_during_migration() {
    let state = State::new();
    fs::write(
        state.0.join("namedrop.sqlite3"),
        b"deliberately invalid database",
    )
    .unwrap();
    fs::create_dir_all(state.0.join("runtime")).unwrap();
    fs::write(state.0.join("runtime/migration.json"), b"pending").unwrap();
    let lock = fs::File::create(state.0.join("runtime/commands.lock")).unwrap();
    lock.lock().unwrap();
    let started = std::time::Instant::now();
    let inputs = [
        json!({"hook_event_name":"SubagentStart","session_id":"parent-session","agent_id":"child","agent_type":"Explore"}).to_string(),
        "not json".to_owned(),
        "x".repeat(namedrop::subagent::SUBAGENT_STDIN_CAP + 1),
    ];
    for input in inputs {
        let mut child = state
            .command()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let _ = child.stdin.take().unwrap().write_all(input.as_bytes());
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
    }
    assert!(
        started.elapsed() < std::time::Duration::from_secs(3),
        "the hook bypasses a held runtime gate"
    );
    assert_eq!(ids(&state, i64::MAX).len(), 0);
    assert_eq!(
        fs::read(state.0.join("namedrop.sqlite3")).unwrap(),
        b"deliberately invalid database"
    );
    let directory = fs::read_dir(state.0.join("subagents/v1"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(
        fs::read_dir(directory).unwrap().count(),
        1,
        "valid start was recorded without opening the DB or update gate"
    );
    let mut child = state
        .command()
        .env_remove("HERDR_PANE_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(br#"{"hook_event_name":"SubagentStart","session_id":"parent-session","agent_id":"ignored"}"#).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success() && output.stdout.is_empty() && output.stderr.is_empty());
}

#[test]
fn blocked_stdin_obeys_five_second_process_deadline() {
    let state = State::new();
    let started = std::time::Instant::now();
    let mut child = state
        .command()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let _held_stdin = child.stdin.take().unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(started.elapsed() >= std::time::Duration::from_millis(4500));
    assert!(started.elapsed() < std::time::Duration::from_secs(7));
    assert!(output.status.success() && output.stdout.is_empty() && output.stderr.is_empty());
}
