//! Model worker behaviour, exercised end to end through fake CLI processes.
//!
//! No test contacts a real provider. The fake CLI records the exact argv,
//! environment, working directory and stdin it was given, which is how the
//! isolation and bounding claims are checked rather than assumed.

use namedrop::environment::EnvironmentSnapshot;
use namedrop::{
    suggest_agent_persona_with_environment as suggest_agent_persona,
    summarize_agent_status_with_environment as summarize_agent_status,
};
use serde_json::{Value, json};
use std::{
    env, fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

/// One fake CLI serves every test. Tests run in parallel threads and the binary
/// override is process wide, so behaviour is selected by a marker the test puts
/// in the snapshot instead of by a second environment variable.
const FAKE_CLI: &str = r#"#!/bin/sh
dir="$(dirname "$0")"
if [ -p /dev/stdin ]; then stdin_kind=PIPE; else stdin_kind=FILE; fi
payload="$(cat)"
mode="$(printf '%s' "$payload" | sed -n 's/.*MODE:\([a-z_]*\).*/\1/p' | head -n 1)"
{
  for argument in "$@"; do printf 'ARG<%s>\n' "$argument"; done
  printf 'CWD<%s>\n' "$(pwd)"
  printf 'HERDR_PANE_ID<%s>\n' "${HERDR_PANE_ID-unset}"
  printf 'HERDR_SOCKET_PATH<%s>\n' "${HERDR_SOCKET_PATH-unset}"
  printf 'HERDR_WORKSPACE_ID<%s>\n' "${HERDR_WORKSPACE_ID-unset}"
  printf 'NAMEDROP_AGENT_SESSION_ID<%s>\n' "${NAMEDROP_AGENT_SESSION_ID-unset}"
  printf 'CLAUDE_CODE_SESSION_ID<%s>\n' "${CLAUDE_CODE_SESSION_ID-unset}"
  printf 'CLAUDE_CODE_CHILD_SESSION<%s>\n' "${CLAUDE_CODE_CHILD_SESSION-unset}"
  printf 'AI_AGENT<%s>\n' "${AI_AGENT-unset}"
  printf 'CLAUDE_CODE_MESSAGING_SOCKET<%s>\n' "${CLAUDE_CODE_MESSAGING_SOCKET-unset}"
  printf 'CLAUDE_CODE_MESSAGING_TOKEN<%s>\n' "${CLAUDE_CODE_MESSAGING_TOKEN-unset}"
  printf 'CLAUDE_CODE_EXECPATH<%s>\n' "${CLAUDE_CODE_EXECPATH-unset}"
  printf 'CLAUDE_PID<%s>\n' "${CLAUDE_PID-unset}"
  printf 'CLAUDECODE<%s>\n' "${CLAUDECODE-unset}"
  printf 'CLAUDE_EFFORT<%s>\n' "${CLAUDE_EFFORT-unset}"
  printf 'MAX_THINKING_TOKENS<%s>\n' "${MAX_THINKING_TOKENS-unset}"
  printf 'STDIN_KIND<%s>\n' "$stdin_kind"
  printf 'STDIN_BYTES<%s>\n' "$(printf '%s' "$payload" | wc -c | tr -d ' ')"
  printf 'STDIN<%s>\n' "$payload"
} >> "$dir/record-$mode.txt"
printf 'x' >> "$dir/calls-$mode.txt"
case "$mode" in
  slow_cancel|slow_big|persona_slow) sleep 30 ;;
  slow_success) sleep 11 ;;
  fail|persona_fail) printf 'worker exploded\n' >&2; exit 3 ;;
  flood) head -c 200000 /dev/zero | tr '\0' 'x'; sleep 30 ;;
esac
if [ -f "$dir/response-$mode.json" ]; then cat "$dir/response-$mode.json"; fi
exit 0
"#;

/// Stands in for `sandbox-exec`: records the profile it was handed, then runs
/// the command. It enforces nothing; it proves what we asked for.
#[cfg(target_os = "macos")]
const FAKE_SANDBOX: &str = r#"#!/bin/sh
dir="$(dirname "$0")"
profile="$2"
shift 2
printf '%s\n' "$profile" > "$dir/last-profile.txt"
SANDBOX_PROFILE="$profile" exec "$@"
"#;

/// Stands in for `codex exec`: Codex writes its final message to the file given
/// by `-o` rather than to stdout.
#[cfg(target_os = "macos")]
const FAKE_CODEX: &str = r#"#!/bin/sh
dir="$(dirname "$0")"
payload="$(cat)"
mode="$(printf '%s' "$payload" | sed -n 's/.*MODE:\([a-z_]*\).*/\1/p' | head -n 1)"
out=""
previous=""
for argument in "$@"; do
  if [ "$previous" = "-o" ]; then out="$argument"; fi
  previous="$argument"
done
{
  printf '%s\n' "${SANDBOX_PROFILE-no-profile}"
  for argument in "$@"; do printf 'ARG<%s>\n' "$argument"; done
  printf 'CWD<%s>\n' "$(pwd)"
  printf 'HERDR_PANE_ID<%s>\n' "${HERDR_PANE_ID-unset}"
  printf 'CLAUDE_CODE_SESSION_ID<%s>\n' "${CLAUDE_CODE_SESSION_ID-unset}"
  printf 'CLAUDE_CODE_MESSAGING_SOCKET<%s>\n' "${CLAUDE_CODE_MESSAGING_SOCKET-unset}"
  printf 'CLAUDE_CODE_MESSAGING_TOKEN<%s>\n' "${CLAUDE_CODE_MESSAGING_TOKEN-unset}"
  printf 'CLAUDE_PID<%s>\n' "${CLAUDE_PID-unset}"
  printf 'MAX_THINKING_TOKENS<%s>\n' "${MAX_THINKING_TOKENS-unset}"
  printf 'STDIN_BYTES<%s>\n' "$(printf '%s' "$payload" | wc -c | tr -d ' ')"
  printf 'STDIN<%s>\n' "$payload"
} >> "$dir/record-$mode.txt"
printf 'x' >> "$dir/calls-$mode.txt"
if [ -n "$out" ] && [ -f "$dir/response-$mode.json" ]; then
  cat "$dir/response-$mode.json" > "$out"
fi
exit 0
"#;

/// Linux uses a real static ELF fake so every Codex test runs under the actual
/// Landlock policy. A shell recorder would necessarily violate that policy.
#[cfg(target_os = "linux")]
fn compile_linux_codex(directory: &std::path::Path) {
    let source = directory.join("codex.c");
    fs::write(&source, r#"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <errno.h>
int main(int argc, char **argv) {
    char payload[1048576], path[8192], directory[4096], cwd[4096], mode[128] = "";
    size_t bytes = fread(payload, 1, sizeof(payload)-1, stdin);
    payload[bytes] = 0;
    char *marker = strstr(payload, "MODE:");
    if (marker) {
        marker += 5;
        size_t n = strspn(marker, "abcdefghijklmnopqrstuvwxyz_");
        if (n >= sizeof(mode)) return 2;
        memcpy(mode, marker, n);
    }
    snprintf(directory, sizeof(directory), "%s", argv[0]);
    char *slash = strrchr(directory, '/');
    if (!slash) return 3;
    *slash = 0;
    snprintf(path, sizeof(path), "%s/record-%s.txt", directory, mode);
    FILE *record = fopen(path, "a");
    if (!record) return 4;
    const char *denied[] = {"private-control", "codex-home/history.jsonl", "codex-home/AGENTS.md"};
    for (size_t i = 0; i < sizeof(denied)/sizeof(*denied); i++) {
        snprintf(path, sizeof(path), "%s/%s", directory, denied[i]);
        FILE *file = fopen(path, "r");
        fprintf(record, "DENIED<%s=%d>\n", denied[i], !file && errno == EACCES);
        if (file) fclose(file);
    }
    FILE *project = fopen(getenv("NAMEDROP_TEST_PROJECT_CANARY"), "r");
    fprintf(record, "DENIED<project=%d>\n", !project && errno == EACCES);
    if (project) fclose(project);
    const char *output = NULL;
    for (int i = 1; i < argc; i++) {
        fprintf(record, "ARG<%s>\n", argv[i]);
        if (i > 1 && !strcmp(argv[i-1], "-o")) output = argv[i];
    }
    fprintf(record, "CWD<%s>\n", getcwd(cwd, sizeof(cwd)));
    const char *keys[] = {"HERDR_PANE_ID", "CLAUDE_CODE_SESSION_ID", "CLAUDE_CODE_MESSAGING_SOCKET", "CLAUDE_CODE_MESSAGING_TOKEN", "CLAUDE_PID", "MAX_THINKING_TOKENS"};
    for (size_t i = 0; i < sizeof(keys)/sizeof(*keys); i++) {
        const char *value = getenv(keys[i]);
        fprintf(record, "%s<%s>\n", keys[i], value ? value : "unset");
    }
    fprintf(record, "STDIN_BYTES<%zu>\nSTDIN<%s>\n", bytes, payload);
    fclose(record);
    snprintf(path, sizeof(path), "%s/calls-%s.txt", directory, mode);
    FILE *calls = fopen(path, "a"); if (!calls) return 5;
    fputc('x', calls); fclose(calls);
    snprintf(path, sizeof(path), "%s/cache/response-%s.json", getenv("CODEX_HOME"), mode);
    FILE *response = fopen(path, "r");
    if (response && output) {
        FILE *result = fopen(output, "w"); if (!result) return 6;
        int c; while ((c = fgetc(response)) != EOF) fputc(c, result);
        fclose(result);
    }
    if (response) fclose(response);
    return 0;
}
"#).expect("native fake source");
    let output = std::process::Command::new("cc")
        .args(["-static", "-O2", "-o"])
        .arg(directory.join("codex"))
        .arg(source)
        .output()
        .expect("C compiler");
    assert!(
        output.status.success(),
        "static native fake build: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(target_os = "linux")]
fn assert_linux_denials(recorded: &str) {
    for name in [
        "private-control",
        "codex-home/history.jsonl",
        "codex-home/AGENTS.md",
        "project",
    ] {
        assert!(
            recorded.contains(&format!("DENIED<{name}=1>")),
            "Codex helper read boundary failed: {recorded}"
        );
    }
}

fn fixture() -> &'static PathBuf {
    static FIXTURE: OnceLock<PathBuf> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let dir = env::temp_dir().join(format!(
            "namedrop-agent-status-model-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("fixture directory");
        #[allow(unused_mut)]
        let mut scripts = vec![("claude", FAKE_CLI)];
        #[cfg(target_os = "macos")]
        scripts.extend([("sandbox-exec", FAKE_SANDBOX), ("codex", FAKE_CODEX)]);
        for (name, body) in scripts {
            let script = dir.join(name);
            fs::write(&script, body).expect("fake CLI");
            fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).expect("fake CLI mode");
        }
        #[cfg(target_os = "linux")]
        {
            compile_linux_codex(&dir);
            fs::create_dir_all(dir.join("codex-home/cache")).expect("fake Codex cache");
            fs::create_dir_all(dir.join("home")).expect("fake home");
            for name in [
                "private-control",
                "codex-home/history.jsonl",
                "codex-home/AGENTS.md",
            ] {
                fs::write(dir.join(name), "unrelated private data").expect("read-denial canary");
            }
            // Safety: initialized once before tests start workers, like the CLI overrides.
            unsafe {
                env::set_var("HOME", dir.join("home"));
                env::set_var("XDG_STATE_HOME", dir.join("home/.local/state"));
                env::set_var("XDG_CONFIG_HOME", dir.join("home/.config"));
                env::set_var("XDG_DATA_HOME", dir.join("home/.local/share"));
                env::set_var("XDG_RUNTIME_DIR", dir.join("home/.runtime"));
                env::set_var("CODEX_HOME", dir.join("codex-home"));
                env::set_var(
                    "NAMEDROP_TEST_PROJECT_CANARY",
                    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"),
                );
            }
        }
        // Safety: set once, before any test spawns a worker.
        unsafe {
            env::set_var("NAMEDROP_AGENT_STATUS_CLAUDE_BIN", dir.join("claude"));
            env::set_var("NAMEDROP_AGENT_STATUS_CODEX_BIN", dir.join("codex"));
            env::set_var(
                "NAMEDROP_AGENT_STATUS_SANDBOX_BIN",
                dir.join("sandbox-exec"),
            );
        }
        dir
    })
}

/// Queues the JSON the fake CLI will print for this mode.
fn respond(mode: &str, body: &Value) {
    let dir = fixture();
    fs::write(
        dir.join(format!("response-{mode}.json")),
        serde_json::to_string(body).expect("response body"),
    )
    .expect("write response");
    #[cfg(target_os = "linux")]
    fs::copy(
        dir.join(format!("response-{mode}.json")),
        dir.join(format!("codex-home/cache/response-{mode}.json")),
    )
    .expect("Codex fixture response");
}

/// The host envelope carries the model's JSON as a string, which is the shape
/// `--output-format json` produces.
fn envelope(inner: Value) -> Value {
    json!({
        "type": "result",
        "result": serde_json::to_string(&inner).expect("inner"),
    })
}

fn record(mode: &str) -> String {
    fs::read_to_string(fixture().join(format!("record-{mode}.txt"))).unwrap_or_default()
}

fn calls(mode: &str) -> usize {
    fs::read(fixture().join(format!("calls-{mode}.txt")))
        .map(|bytes| bytes.len())
        .unwrap_or(0)
}

fn snapshot_with(mode: &str, host: &str, origin_excerpt: &str, peer_excerpt: &str) -> Value {
    fixture();
    let label = format!("lead MODE:{mode}");
    json!({
        "origin": {
            "workspace_id": "w44",
            "pane_id": "w44:p1",
            "terminal_id": "term-1",
            "session_id": "sess-1",
            "host": host,
            "cwd": "/tmp",
            "label": label,
            "status": "working",
        },
        "observed_at_ms": 1_700_000_000_000u64,
        "panes": [
            {
                "pane_id": "w44:p1",
                "terminal_id": "term-1",
                "session_id": "sess-1",
                "label": label,
                "host": host,
                "status": "working",
                "excerpt": origin_excerpt,
            },
            {
                "pane_id": "w44:p2",
                "terminal_id": "term-2",
                "session_id": "sess-2",
                "label": "reviewer",
                "host": host,
                "status": "working",
                "excerpt": peer_excerpt,
            }
        ],
        "waits": [],
        "handoffs_available": true,
        "warnings": [],
        "omitted_panes": 0,
    })
}

fn snapshot(mode: &str) -> Value {
    snapshot_with(mode, "claude", "editing the parser", "running the tests")
}

fn idle() -> AtomicBool {
    AtomicBool::new(false)
}

/// Waits until this mode's worker has actually started and returns that moment.
///
/// Workers are serialized, so any assertion about how long a worker ran has to
/// start counting here rather than before the call, or it measures whichever
/// worker a parallel test happened to be running.
fn started(mode: &str) -> Instant {
    for _ in 0..1_500 {
        if calls(mode) > 0 {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    Instant::now()
}

#[test]
fn summarizes_the_origin_and_known_peers() {
    respond(
        "ok",
        &envelope(json!({
            "origin_description": "Editing the parser.",
            "peers": [{"pane_id": "w44:p2", "description": "Running the test suite."}],
        })),
    );

    let summary = summarize_agent_status(&snapshot("ok"), &idle(), &EnvironmentSnapshot::capture())
        .expect("summary");

    assert_eq!(summary["origin_description"], "Editing the parser.");
    assert_eq!(summary["peers"][0]["pane_id"], "w44:p2");
    assert_eq!(
        summary["peers"][0]["description"],
        "Running the test suite."
    );
    assert_eq!(summary["model"], "claude-haiku-4-5");
    assert!(
        summary["summarized_at_ms"].as_u64().unwrap_or(0) > 0,
        "summary must be stamped by our clock, got {summary}"
    );
}

#[test]
fn accepts_a_structured_result_object() {
    respond(
        "structured",
        &json!({
            "type": "result",
            "result": {"origin_description": "Reading the plan.", "peers": []},
        }),
    );

    let summary = summarize_agent_status(
        &snapshot("structured"),
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect("summary");

    assert_eq!(summary["origin_description"], "Reading the plan.");
    assert_eq!(summary["peers"].as_array().expect("peers").len(), 0);
}

#[test]
fn drops_pane_ids_the_snapshot_never_mentioned() {
    respond(
        "unknown_peer",
        &envelope(json!({
            "origin_description": "Editing the parser.",
            "peers": [
                {"pane_id": "w44:p2", "description": "Running the test suite."},
                {"pane_id": "w99:p9", "description": "Invented pane."},
                {"pane_id": "w44:p1", "description": "The origin restated."},
                {"pane_id": "w44:p2", "description": "A duplicate."}
            ],
        })),
    );

    let summary = summarize_agent_status(
        &snapshot("unknown_peer"),
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect("summary");

    let peers = summary["peers"].as_array().expect("peers");
    assert_eq!(
        peers.len(),
        1,
        "only the known peer survives, got {summary}"
    );
    assert_eq!(peers[0]["pane_id"], "w44:p2");
    assert_eq!(peers[0]["description"], "Running the test suite.");
}

#[test]
fn strips_control_characters_and_caps_description_length() {
    let long = "b".repeat(500);
    respond(
        "dirty",
        &envelope(json!({
            "origin_description": "Red\u{1b}[31m alert\u{7}\nsecond line",
            "peers": [{"pane_id": "w44:p2", "description": long}],
        })),
    );

    let summary =
        summarize_agent_status(&snapshot("dirty"), &idle(), &EnvironmentSnapshot::capture())
            .expect("summary");

    let origin = summary["origin_description"].as_str().expect("origin");
    assert!(
        !origin.contains('\u{1b}') && !origin.contains('\u{7}') && !origin.contains('\n'),
        "control characters must not reach the panel, got {origin:?}"
    );
    let description = summary["peers"][0]["description"].as_str().expect("peer");
    assert_eq!(
        description.chars().count(),
        200,
        "descriptions are capped at 200 characters"
    );
}

#[test]
fn invokes_the_same_host_with_verified_isolation_controls() {
    respond(
        "record",
        &envelope(json!({"origin_description": "Working.", "peers": []})),
    );

    summarize_agent_status(
        &snapshot("record"),
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect("summary");
    let recorded = record("record");

    for expected in [
        "ARG<--print>\n",
        "ARG<--model>\nARG<claude-haiku-4-5>\n",
        // An empty value is the control that disables every built-in tool.
        "ARG<--tools>\nARG<>\n",
        "ARG<--strict-mcp-config>\n",
        // No settings sources means no hooks and no project instructions.
        "ARG<--setting-sources>\nARG<>\n",
        "ARG<--disable-slash-commands>\n",
        "ARG<--no-session-persistence>\n",
        "ARG<--permission-prompts>\nARG<none>\n",
        "ARG<--max-budget-usd>\n",
        "ARG<--json-schema>\n",
        "ARG<--system-prompt>\n",
    ] {
        assert!(
            recorded.contains(expected),
            "worker must be launched with {expected:?}, got:\n{recorded}"
        );
    }

    for forbidden in ["--resume", "--continue", "--fork-session", "--dangerously"] {
        assert!(
            !recorded.contains(forbidden),
            "worker must never {forbidden}, got:\n{recorded}"
        );
    }

    // The worker carries no collaboration identity, so it cannot be mistaken
    // for the registered session or reach its mailbox.
    for stripped in [
        "HERDR_PANE_ID<unset>",
        "HERDR_SOCKET_PATH<unset>",
        "HERDR_WORKSPACE_ID<unset>",
        "NAMEDROP_AGENT_SESSION_ID<unset>",
    ] {
        assert!(
            recorded.contains(stripped),
            "worker environment must be stripped of {stripped:?}, got:\n{recorded}"
        );
    }

    let repository = env::current_dir().expect("cwd");
    assert!(
        !recorded.contains(&format!("CWD<{}>", repository.display())),
        "worker must not run in the project directory, got:\n{recorded}"
    );
}

#[test]
fn bounds_the_model_input_and_keeps_origin_context() {
    respond(
        "big",
        &envelope(json!({"origin_description": "Working.", "peers": []})),
    );
    let origin_excerpt = (0..400)
        .map(|line| format!("origin line {line} of the transcript"))
        .collect::<Vec<_>>()
        .join("\n");
    let peer_excerpt = (0..400)
        .map(|line| format!("peer line {line} padded out to take real space"))
        .collect::<Vec<_>>()
        .join("\n");

    let snapshot = snapshot_with("big", "claude", &origin_excerpt, &peer_excerpt);
    summarize_agent_status(&snapshot, &idle(), &EnvironmentSnapshot::capture()).expect("summary");

    let recorded = record("big");
    let bytes: usize = recorded
        .lines()
        .find_map(|line| {
            line.strip_prefix("STDIN_BYTES<")?
                .strip_suffix('>')?
                .parse()
                .ok()
        })
        .expect("recorded stdin size");
    assert!(
        bytes <= 24 * 1024,
        "model input must stay inside its budget, got {bytes} bytes"
    );
    assert!(
        recorded.contains("origin line 399"),
        "the origin's most recent context is allocated first, got {bytes} bytes:\n{recorded}"
    );
    assert!(
        !recorded.contains("origin line 0 "),
        "only the recent tail of a transcript is sent"
    );
}

#[test]
fn reuses_a_valid_summary_instead_of_spending_again() {
    respond(
        "cache",
        &envelope(json!({"origin_description": "Working.", "peers": []})),
    );
    let snapshot = snapshot("cache");

    let first = summarize_agent_status(&snapshot, &idle(), &EnvironmentSnapshot::capture())
        .expect("first summary");
    let second = summarize_agent_status(&snapshot, &idle(), &EnvironmentSnapshot::capture())
        .expect("second summary");

    assert_eq!(calls("cache"), 1, "an unchanged snapshot must not respawn");
    assert_eq!(first, second);
}

#[test]
fn a_changed_snapshot_is_summarized_again() {
    respond(
        "recollect",
        &envelope(json!({"origin_description": "Working.", "peers": []})),
    );

    summarize_agent_status(
        &snapshot_with("recollect", "claude", "first state", "peer"),
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect("first summary");
    summarize_agent_status(
        &snapshot_with("recollect", "claude", "second state", "peer"),
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect("second summary");

    assert_eq!(
        calls("recollect"),
        2,
        "meaningful change must not serve a stale summary"
    );
}

#[test]
fn a_new_collection_time_alone_does_not_respawn_the_worker() {
    respond(
        "restamp",
        &envelope(json!({"origin_description": "Working.", "peers": []})),
    );
    let mut first = snapshot_with("restamp", "claude", "same state", "peer");
    let mut second = first.clone();
    first["observed_at_ms"] = json!(1_700_000_000_000u64);
    second["observed_at_ms"] = json!(1_700_000_999_999u64);

    summarize_agent_status(&first, &idle(), &EnvironmentSnapshot::capture())
        .expect("first summary");
    summarize_agent_status(&second, &idle(), &EnvironmentSnapshot::capture())
        .expect("second summary");

    assert_eq!(
        calls("restamp"),
        1,
        "collection timestamps are excluded from the cache digest"
    );
}

#[test]
fn a_replaced_session_never_reuses_the_previous_summary() {
    respond(
        "resession",
        &envelope(json!({"origin_description": "Working.", "peers": []})),
    );
    let first = snapshot_with("resession", "claude", "same state", "peer");
    let mut second = first.clone();
    second["origin"]["session_id"] = json!("sess-replaced");

    summarize_agent_status(&first, &idle(), &EnvironmentSnapshot::capture())
        .expect("first summary");
    summarize_agent_status(&second, &idle(), &EnvironmentSnapshot::capture())
        .expect("second summary");

    assert_eq!(
        calls("resession"),
        2,
        "a replaced session must not inherit the previous summary"
    );
}

/// The whole prompt is staged in a file. A pipe's capacity is not portable, so
/// a child that has not read its stdin must never be able to block the parent
/// before its cancellation loop starts.
#[test]
fn the_worker_prompt_is_staged_in_a_file_not_a_pipe() {
    respond(
        "stdinkind",
        &envelope(json!({"origin_description": "Working.", "peers": []})),
    );

    summarize_agent_status(
        &snapshot("stdinkind"),
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect("summary");

    let recorded = record("stdinkind");
    assert!(
        recorded.contains("STDIN_KIND<FILE>"),
        "the prompt must not depend on pipe capacity, got:\n{recorded}"
    );
}

/// The near-limit companion to the check above: a payload at the top of the
/// input budget must still cancel promptly against a child that is not reading.
#[test]
fn a_near_limit_prompt_still_cancels_promptly() {
    let cancelled = AtomicBool::new(false);
    let requested_at: Mutex<Option<Instant>> = Mutex::new(None);
    // Comfortably past any small pipe buffer, and close to the 24 KiB cap.
    let bulky = (0..900)
        .map(|line| format!("origin transcript line {line} with padding to add bytes"))
        .collect::<Vec<_>>()
        .join("\n");
    let snapshot = snapshot_with("slow_big", "claude", &bulky, &bulky);

    let error = thread::scope(|scope| {
        scope.spawn(|| {
            for _ in 0..1_500 {
                if calls("slow_big") > 0 {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            *requested_at.lock().expect("clock") = Some(Instant::now());
            cancelled.store(true, Ordering::Relaxed);
        });
        summarize_agent_status(&snapshot, &cancelled, &EnvironmentSnapshot::capture())
            .expect_err("cancelled")
    });

    assert!(error.contains("cancelled"), "got {error:?}");
    let waited = requested_at
        .lock()
        .expect("clock")
        .expect("the worker must have started")
        .elapsed();
    assert!(
        waited < Duration::from_secs(3),
        "a near-limit prompt must not strand cancellation, took {waited:?}"
    );
    let recorded = record("slow_big");
    assert!(recorded.contains("STDIN_KIND<FILE>"), "got:\n{recorded}");
}

/// An output cap has to end the work, not merely reject the result once the
/// worker finally exits on its own.
#[test]
fn an_oversized_worker_is_stopped_while_it_is_still_running() {
    let cancelled = idle();
    // Timed from the moment this worker starts. One worker runs at a time, so
    // timing from before the call would measure another test's worker instead.
    let started_at: Mutex<Option<Instant>> = Mutex::new(None);

    let error = thread::scope(|scope| {
        scope.spawn(|| {
            *started_at.lock().expect("clock") = Some(started("flood"));
        });
        summarize_agent_status(
            &snapshot("flood"),
            &cancelled,
            &EnvironmentSnapshot::capture(),
        )
        .expect_err("an oversized worker must not succeed")
    });

    assert!(
        error.contains("exceeded its limit"),
        "the cap must be reported, got {error:?}"
    );
    let ran_for = started_at
        .lock()
        .expect("clock")
        .expect("the worker must have started")
        .elapsed();
    assert!(
        ran_for < Duration::from_secs(5),
        "the cap must terminate the worker before it exits naturally, ran for {ran_for:?}"
    );
}

#[test]
fn a_slow_worker_can_finish_after_the_former_deadline() {
    respond(
        "slow_success",
        &envelope(json!({"origin_description": "Delayed but valid summary.", "peers": []})),
    );
    let summary = summarize_agent_status(
        &snapshot("slow_success"),
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect("a running worker must not be cancelled just because ten seconds passed");
    assert_eq!(summary["origin_description"], "Delayed but valid summary.");
}

#[test]
fn cancellation_stops_a_running_worker() {
    let cancelled = AtomicBool::new(false);
    // Timed from the cancellation request itself: one worker runs at a time, so
    // a parallel test's worker would otherwise be counted as latency here.
    let requested_at: Mutex<Option<Instant>> = Mutex::new(None);

    let error = thread::scope(|scope| {
        scope.spawn(|| {
            for _ in 0..1_500 {
                if calls("slow_cancel") > 0 {
                    break;
                }
                thread::sleep(Duration::from_millis(20));
            }
            *requested_at.lock().expect("clock") = Some(Instant::now());
            cancelled.store(true, Ordering::Relaxed);
        });
        summarize_agent_status(
            &snapshot("slow_cancel"),
            &cancelled,
            &EnvironmentSnapshot::capture(),
        )
        .expect_err("a cancelled worker must not succeed")
    });

    assert!(
        error.contains("cancelled"),
        "cancellation must be reported, got {error:?}"
    );
    let waited = requested_at
        .lock()
        .expect("clock")
        .expect("the worker must have started")
        .elapsed();
    assert!(
        waited < Duration::from_secs(3),
        "cancellation must stop the worker promptly, took {waited:?}"
    );
}

#[test]
fn an_already_cancelled_request_never_spawns_a_worker() {
    let cancelled = AtomicBool::new(true);

    let error = summarize_agent_status(
        &snapshot("never"),
        &cancelled,
        &EnvironmentSnapshot::capture(),
    )
    .expect_err("a cancelled request must not summarize");

    assert!(error.contains("cancelled"), "got {error:?}");
    assert_eq!(calls("never"), 0, "no worker may be spawned");
}

#[test]
fn a_failing_worker_falls_back_instead_of_retrying() {
    let error = summarize_agent_status(&snapshot("fail"), &idle(), &EnvironmentSnapshot::capture())
        .expect_err("a failing worker must not succeed");

    assert!(
        error.contains("model worker failed"),
        "the failure must be reported, got {error:?}"
    );
    assert_eq!(calls("fail"), 1, "there is no automatic retry");
}

#[test]
fn malformed_output_is_rejected() {
    respond("garbage", &json!("not the expected envelope"));

    let error = summarize_agent_status(
        &snapshot("garbage"),
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect_err("malformed output must not be rendered");

    assert!(
        error.contains("malformed") || error.contains("unexpected shape"),
        "got {error:?}"
    );
}

#[test]
fn an_oversized_response_is_rejected() {
    respond(
        "oversized",
        &json!({"type": "result", "result": "x".repeat(5000)}),
    );

    let error = summarize_agent_status(
        &snapshot("oversized"),
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect_err("an oversized response must not be rendered");

    assert!(error.contains("exceeded its limit"), "got {error:?}");
}

#[test]
fn a_denied_model_is_reported_rather_than_upgraded() {
    respond(
        "denied",
        &json!({
            "type": "result",
            "is_error": true,
            "result": "model claude-haiku-4-5 is not available for this account",
        }),
    );

    let error = summarize_agent_status(
        &snapshot("denied"),
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect_err("a denied model must not be rendered");

    assert!(
        error.contains("reported an error") && error.contains("not available"),
        "the host's reason must survive, got {error:?}"
    );
    assert_eq!(calls("denied"), 1, "a denied model must not be retried");
}

#[test]
fn an_empty_origin_description_is_rejected() {
    respond(
        "empty",
        &envelope(json!({"origin_description": "   ", "peers": []})),
    );

    let error =
        summarize_agent_status(&snapshot("empty"), &idle(), &EnvironmentSnapshot::capture())
            .expect_err("an empty description must not be rendered");

    assert!(error.contains("no origin description"), "got {error:?}");
}

/// Codex advertises tools that no configuration key removes, so it is
/// contained instead. This asserts the invocation we ask for; that the
/// containment actually blocks a tool call is live evidence recorded in
/// `.unlazy/agent-status/reports/model.md`, not something a fake can show.
#[test]
fn codex_runs_contained_by_a_child_only_sandbox() {
    respond(
        "codexrun",
        &json!({"origin_description": "Editing the parser.", "peers": []}),
    );

    let summary = summarize_agent_status(
        &snapshot_with("codexrun", "codex", "editing the parser", "peer"),
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect("summary");
    assert_eq!(summary["model"], "gpt-5.6-luna");
    assert_eq!(summary["origin_description"], "Editing the parser.");

    let recorded = record("codexrun");
    #[cfg(target_os = "linux")]
    assert_linux_denials(&recorded);
    #[cfg(target_os = "macos")]
    {
        // The wrapper must deny starting any process but the CLI itself.
        assert!(
            recorded.contains("(deny process-exec*)"),
            "the worker must be denied process execution, got:\n{recorded}"
        );
        assert!(
            recorded.contains("(allow process-exec (literal \"") && recorded.contains("codex"),
            "only the CLI itself may be executed, got:\n{recorded}"
        );
        // Unrelated reads are denied too, so the worker cannot browse the user's
        // files while summarizing.
        assert!(
            recorded.contains("(deny file-read*)"),
            "the worker must be denied unrelated reads, got:\n{recorded}"
        );
        let repository = env::current_dir().expect("cwd");
        assert!(
            !recorded.contains(&format!("(subpath \"{}\")", repository.display())),
            "the project directory must never be readable, got:\n{recorded}"
        );
        // Blanket temp roots would re-admit unrelated temporary files, and a whole
        // credential home would re-admit history, transcripts, rules and skills.
        for blanket in [
            "(subpath \"/tmp\")",
            "(subpath \"/private/tmp\")",
            "(subpath \"/var\")",
            "(subpath \"/private/var\")",
        ] {
            assert!(
                !recorded.contains(blanket),
                "the read allowance must not include the blanket path {blanket}, got:\n{recorded}"
            );
        }
        let home = env::var("HOME").expect("HOME");
        for denied in ["sessions", "history.jsonl", "skills", "hooks", "AGENTS.md"] {
            assert!(
                !recorded.contains(&format!("{home}/.codex/{denied}")),
                "the worker must not be able to read {denied}, got:\n{recorded}"
            );
        }
        assert!(
            !recorded.contains(&format!("(subpath \"{home}/.codex\")")),
            "the whole credential home must not be readable, got:\n{recorded}"
        );
        // Auth still works: the credential files are named one by one.
        assert!(
            recorded.contains(&format!("(literal \"{home}/.codex/auth.json\")")),
            "normal authentication must be preserved, got:\n{recorded}"
        );
    }
    for expected in [
        "ARG<--ignore-user-config>\n",
        "ARG<--ignore-rules>\n",
        "ARG<--ephemeral>\n",
        "ARG<-s>\nARG<read-only>\n",
        // Otherwise the startup snapshot tries to spawn a shell.
        "ARG<--disable>\nARG<shell_snapshot>\n",
        "ARG<-c>\nARG<project_doc_max_bytes=0>\n",
        // Captured requests asked for reasoning effort "medium" without this.
        "ARG<-c>\nARG<model_reasoning_effort=\"low\">\n",
        "ARG<-m>\nARG<gpt-5.6-luna>\n",
        "ARG<--output-schema>\n",
    ] {
        assert!(
            recorded.contains(expected),
            "Codex worker must be launched with {expected:?}, got:\n{recorded}"
        );
    }
    assert!(
        !recorded.contains("--dangerously-bypass"),
        "the worker must never bypass the sandbox, got:\n{recorded}"
    );
}

/// Exercises the generated read rules against the real sandbox, so the profile
/// is checked by enforcement rather than by string matching alone.
///
/// The canary keeps the read rules verbatim but drops the execution denial,
/// because otherwise the canary process could not start at all. Local and free;
/// no provider is contacted.
#[test]
fn the_generated_read_profile_denies_unrelated_files() {
    if !cfg!(target_os = "macos") {
        return;
    }
    respond(
        "canary",
        &json!({"origin_description": "Working.", "peers": []}),
    );
    summarize_agent_status(
        &snapshot_with("canary", "codex", "work", "work"),
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect("summary");

    let dir = fixture();
    let profile = fs::read_to_string(dir.join("last-profile.txt")).expect("generated profile");
    let canary: String = profile
        .lines()
        .filter(|line| !line.contains("process-exec"))
        .collect::<Vec<_>>()
        .join("\n");
    let canary_path = dir.join("canary.sb");
    fs::write(&canary_path, &canary).expect("canary profile");

    let readable = |path: &std::path::Path| {
        std::process::Command::new("/usr/bin/sandbox-exec")
            .arg("-f")
            .arg(&canary_path)
            .arg("/bin/cat")
            .arg(path)
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    };

    // Negative: a file in the project checkout, which the worker has no
    // business reading while it summarizes.
    let project_file = env::current_dir().expect("cwd").join("Cargo.toml");
    assert!(
        !readable(&project_file),
        "the project checkout must not be readable under the generated profile:\n{canary}"
    );
    // Control: a file the worker legitimately needs, proving the canary itself
    // works and the profile is not simply denying everything.
    let allowed = dir.join("control.txt");
    fs::write(&allowed, "control").expect("control file");
    assert!(
        readable(&allowed),
        "the worker's own directory must stay readable:\n{canary}"
    );
}

/// A summary is paraphrase of text already supplied, so the worker must not be
/// given a large reasoning allowance. Captured requests showed the CLI asking
/// for a 31,999-token thinking budget by default, which neither `--effort` nor
/// the prompt's own length instruction bounds.
#[test]
fn the_claude_worker_disables_extended_thinking() {
    respond(
        "thinking",
        &envelope(json!({"origin_description": "Working.", "peers": []})),
    );

    summarize_agent_status(
        &snapshot("thinking"),
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect("summary");

    let recorded = record("thinking");
    assert!(
        recorded.contains("MAX_THINKING_TOKENS<0>"),
        "the worker must not be given a reasoning allowance, got:\n{recorded}"
    );
}

/// Seeds a hostile parent environment and checks both workers against it.
///
/// The variables under test are normally absent on a build machine, so a test
/// that only reads the ambient environment passes even when the stripping is
/// removed. This one supplies the values itself, by re-running this single test
/// in a subprocess with them set, which also avoids mutating process-global
/// environment while the rest of the suite runs in parallel threads.
#[test]
fn a_seeded_parent_environment_is_stripped_and_overridden() {
    const MARKER: &str = "NAMEDROP_TEST_SEEDED_PARENT";
    const SEEDED_THINKING: &str = "99999";

    if env::var(MARKER).is_err() {
        let status = std::process::Command::new(env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "a_seeded_parent_environment_is_stripped_and_overridden",
                "--nocapture",
            ])
            .env(MARKER, "1")
            // A high inherited budget: the Claude worker must override it, not
            // merely decline to set one.
            .env("MAX_THINKING_TOKENS", SEEDED_THINKING)
            // Live handles to a parent session, which neither worker may carry.
            .env("CLAUDE_CODE_MESSAGING_SOCKET", "/tmp/synthetic-parent.sock")
            .env("CLAUDE_CODE_MESSAGING_TOKEN", "synthetic-parent-token")
            .env("CLAUDE_CODE_EXECPATH", "/synthetic/execpath")
            .env("CLAUDE_PID", "4242")
            .env("CLAUDECODE", "1")
            .env("CLAUDE_EFFORT", "xhigh")
            .output()
            .expect("re-run this test with a seeded parent environment");
        assert!(
            status.status.success(),
            "seeded run failed:\n{}{}",
            String::from_utf8_lossy(&status.stdout),
            String::from_utf8_lossy(&status.stderr)
        );
        return;
    }

    // Seeded child: every variable above is present in this process.
    respond(
        "seeded",
        &envelope(json!({"origin_description": "Working.", "peers": []})),
    );
    respond(
        "seededcodex",
        &json!({"origin_description": "Working.", "peers": []}),
    );

    summarize_agent_status(
        &snapshot("seeded"),
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect("claude summary");
    summarize_agent_status(
        &snapshot_with("seededcodex", "codex", "work", "work"),
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect("codex summary");

    let claude = record("seeded");
    let codex = record("seededcodex");

    // Both hosts drop the parent's IPC and session handles.
    for (host, recorded) in [("claude", &claude), ("codex", &codex)] {
        for marker in [
            "CLAUDE_CODE_MESSAGING_SOCKET<unset>",
            "CLAUDE_CODE_MESSAGING_TOKEN<unset>",
            "CLAUDE_PID<unset>",
        ] {
            assert!(
                recorded.contains(marker),
                "the {host} worker must strip {marker:?} even when the parent sets it, got:\n{recorded}"
            );
        }
        assert!(
            !recorded.contains("synthetic-parent-token"),
            "the {host} worker must not carry the parent's token, got:\n{recorded}"
        );
    }

    // Claude overrides the inherited budget rather than inheriting it.
    assert!(
        claude.contains("MAX_THINKING_TOKENS<0>"),
        "the Claude worker must override an inherited budget of {SEEDED_THINKING}, got:\n{claude}"
    );
    // Codex reasoning configuration is deliberately left alone.
}

#[test]
fn both_hosts_are_stripped_of_inherited_agent_session_identity() {
    respond(
        "identity",
        &envelope(json!({"origin_description": "Working.", "peers": []})),
    );

    summarize_agent_status(
        &snapshot("identity"),
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect("summary");
    let recorded = record("identity");

    // Observed on a live pane: the invoking agent exports its own session
    // markers, which would otherwise mark the worker as that agent's child.
    for stripped in [
        "CLAUDE_CODE_SESSION_ID<unset>",
        "CLAUDE_CODE_CHILD_SESSION<unset>",
        "AI_AGENT<unset>",
    ] {
        assert!(
            recorded.contains(stripped),
            "worker environment must be stripped of {stripped:?}, got:\n{recorded}"
        );
    }
}

#[test]
fn an_unsupported_host_is_reported_rather_than_substituted() {
    let error = summarize_agent_status(
        &snapshot_with("shell", "bash", "work", "work"),
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect_err("a shell pane has no agent CLI");

    assert!(error.contains("unavailable for this host"), "got {error:?}");
    assert_eq!(calls("shell"), 0, "no other provider may be used");
}

#[test]
fn a_pane_with_no_agent_is_reported() {
    let mut snapshot = snapshot("hostless");
    snapshot["origin"]["host"] = Value::Null;

    let error = summarize_agent_status(&snapshot, &idle(), &EnvironmentSnapshot::capture())
        .expect_err("no host, no summary");

    assert!(error.contains("no known agent"), "got {error:?}");
}

#[test]
fn a_snapshot_without_an_origin_is_rejected() {
    let error = summarize_agent_status(
        &json!({"panes": []}),
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect_err("a snapshot must carry its origin");

    assert!(error.contains("missing its origin"), "got {error:?}");
}

// Persona suggestions: the same isolated worker, asked a different question.

/// Names the agent and selects the fake CLI's behaviour for this test.
fn persona_alias(mode: &str) -> String {
    fixture();
    format!("researcher MODE:{mode}")
}

/// The JSON the worker was actually handed. Claude is given the payload alone;
/// Codex is given it wrapped in the same prompt, so the line is found rather
/// than assumed to be the whole of stdin.
fn recorded_payload(mode: &str) -> Value {
    let recorded = record(mode);
    let payload = recorded
        .lines()
        .map(|line| line.trim_start_matches("STDIN<").trim_end_matches('>'))
        .find(|line| line.starts_with('{'))
        .unwrap_or_else(|| panic!("recorded payload, got:\n{recorded}"));
    serde_json::from_str(payload).unwrap_or_else(|_| panic!("payload JSON, got:\n{payload}"))
}

#[test]
fn suggests_a_persona_from_the_name_and_the_draft() {
    respond(
        "persona_ok",
        &envelope(json!({"description": "Researches options and writes up a recommendation."})),
    );

    let suggestion = suggest_agent_persona(
        &persona_alias("persona_ok"),
        "digs into options and writes them up",
        "claude",
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect("suggestion");

    assert_eq!(
        suggestion,
        "Researches options and writes up a recommendation."
    );
}

#[test]
fn suggests_a_persona_on_codex_under_the_same_containment() {
    respond(
        "persona_codex",
        &json!({"description": "Reviews changes for correctness."}),
    );

    let suggestion = suggest_agent_persona(
        &persona_alias("persona_codex"),
        "reviews changes",
        "codex",
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect("suggestion");

    assert_eq!(suggestion, "Reviews changes for correctness.");
    let recorded = record("persona_codex");
    #[cfg(target_os = "linux")]
    assert_linux_denials(&recorded);
    #[cfg(target_os = "macos")]
    for rule in ["(deny process-exec*)", "(deny file-read*)"] {
        assert!(
            recorded.contains(rule),
            "missing Mac containment: {recorded}"
        );
    }
    assert_eq!(
        recorded_payload("persona_codex"),
        json!({"name": "researcher MODE:persona_codex", "draft": "reviews changes"}),
        "only the supplied name and draft may reach Codex, got:\n{recorded}"
    );
    for expected in [
        "ARG<-m>\nARG<gpt-5.6-luna>\n",
        "ARG<--ignore-user-config>\n",
        "ARG<--ephemeral>\n",
        "ARG<--output-schema>\n",
    ] {
        assert!(
            recorded.contains(expected),
            "the persona worker must keep Codex containment {expected:?}, got:\n{recorded}"
        );
    }
}

#[test]
fn only_the_supplied_name_and_draft_reach_the_persona_worker() {
    respond(
        "persona_input",
        &envelope(json!({"description": "General purpose agent."})),
    );

    suggest_agent_persona(
        &persona_alias("persona_input"),
        "handles scheduling",
        "claude",
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect("suggestion");

    let recorded = record("persona_input");
    assert_eq!(
        recorded_payload("persona_input"),
        json!({"name": "researcher MODE:persona_input", "draft": "handles scheduling"}),
        "the payload carries the two supplied fields and nothing else, got:\n{recorded}"
    );
    // No pane, no transcript, no status vocabulary: this worker is asked only
    // about the persona, so a status answer cannot even be requested.
    assert!(
        !recorded.contains("origin_description") && !recorded.contains("pane_id"),
        "the persona worker must not be given the status prompt or schema, got:\n{recorded}"
    );
    // Same isolation controls as a summary: no identity, no project directory.
    for stripped in [
        "HERDR_PANE_ID<unset>",
        "HERDR_SOCKET_PATH<unset>",
        "HERDR_WORKSPACE_ID<unset>",
        "NAMEDROP_AGENT_SESSION_ID<unset>",
        "CLAUDE_CODE_SESSION_ID<unset>",
        "MAX_THINKING_TOKENS<0>",
    ] {
        assert!(
            recorded.contains(stripped),
            "the persona worker must be launched with {stripped:?}, got:\n{recorded}"
        );
    }
    for expected in [
        "ARG<--model>\nARG<claude-haiku-4-5>\n",
        "ARG<--tools>\nARG<>\n",
        "ARG<--setting-sources>\nARG<>\n",
        "ARG<--no-session-persistence>\n",
        // The persona question and the persona schema, not the status ones.
        "persona description for a named agent",
        r#"ARG<{"additionalProperties":false,"properties":{"description":{"description":"Plain prose describing the intended work, not JSON, code or a serialized object.","type":"string"}}"#,
    ] {
        assert!(
            recorded.contains(expected),
            "the persona worker must be launched with {expected:?}, got:\n{recorded}"
        );
    }
    let repository = env::current_dir().expect("cwd");
    assert!(
        !recorded.contains(&format!("CWD<{}>", repository.display())),
        "the persona worker must not run in the project directory, got:\n{recorded}"
    );
}

/// Suggesting again is an explicit user action on the current text, so it must
/// actually generate rather than replay a stored answer.
#[test]
fn suggesting_again_generates_again() {
    respond(
        "persona_again",
        &envelope(json!({"description": "General purpose agent."})),
    );
    let alias = persona_alias("persona_again");

    suggest_agent_persona(
        &alias,
        "same draft",
        "claude",
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect("first suggestion");
    suggest_agent_persona(
        &alias,
        "same draft",
        "claude",
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect("second suggestion");

    assert_eq!(
        calls("persona_again"),
        2,
        "a persona suggestion must not be served from the summary cache"
    );
}

#[test]
fn cancellation_stops_a_running_persona_worker() {
    let cancelled = AtomicBool::new(false);
    let requested_at: Mutex<Option<Instant>> = Mutex::new(None);

    let error = thread::scope(|scope| {
        scope.spawn(|| {
            *requested_at.lock().expect("clock") = Some(started("persona_slow"));
            cancelled.store(true, Ordering::Relaxed);
        });
        suggest_agent_persona(
            &persona_alias("persona_slow"),
            "draft",
            "claude",
            &cancelled,
            &EnvironmentSnapshot::capture(),
        )
        .expect_err("a cancelled suggestion must not succeed")
    });

    assert!(error.contains("cancelled"), "got {error:?}");
    let waited = requested_at
        .lock()
        .expect("clock")
        .expect("the worker must have started")
        .elapsed();
    assert!(
        waited < Duration::from_secs(3),
        "cancellation must stop the worker promptly, took {waited:?}"
    );
}

#[test]
fn an_already_cancelled_persona_request_never_spawns_a_worker() {
    let error = suggest_agent_persona(
        &persona_alias("persona_never"),
        "draft",
        "claude",
        &AtomicBool::new(true),
        &EnvironmentSnapshot::capture(),
    )
    .expect_err("a cancelled request must not suggest");

    assert!(error.contains("cancelled"), "got {error:?}");
    assert_eq!(calls("persona_never"), 0, "no worker may be spawned");
}

#[test]
fn a_failing_persona_worker_is_reported_without_retrying() {
    let error = suggest_agent_persona(
        &persona_alias("persona_fail"),
        "draft",
        "claude",
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect_err("a failing worker must not succeed");

    assert!(
        error.contains("model worker failed"),
        "the failure must be reported, got {error:?}"
    );
    assert_eq!(calls("persona_fail"), 1, "there is no automatic retry");
}

#[test]
fn a_persona_suggestion_is_sanitized_and_capped() {
    respond(
        "persona_dirty",
        &envelope(json!({
            "description": format!("Red\u{1b}[31m alert\nsecond line {}", "b".repeat(1200)),
        })),
    );

    let suggestion = suggest_agent_persona(
        &persona_alias("persona_dirty"),
        "draft",
        "claude",
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect("suggestion");

    assert!(
        !suggestion.contains('\u{1b}') && !suggestion.contains('\n'),
        "control characters must not reach the editor, got {suggestion:?}"
    );
    assert_eq!(
        suggestion.chars().count(),
        1000,
        "a persona suggestion is capped at 1000 characters"
    );
}

#[test]
fn an_empty_persona_suggestion_is_rejected() {
    respond("persona_empty", &envelope(json!({"description": "   "})));

    let error = suggest_agent_persona(
        &persona_alias("persona_empty"),
        "draft",
        "claude",
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect_err("an empty suggestion must not be offered");

    assert!(error.contains("no persona description"), "got {error:?}");
}

#[test]
fn a_persona_description_must_not_be_a_serialized_json_document() {
    // Captured from a real Claude response: the schema field itself contained
    // an encoded object, which otherwise appeared verbatim in the editor.
    let long_object = json!({"description": "x".repeat(1100)}).to_string();
    for (mode, description) in [
        (
            "persona_nested_object",
            r#"{"description": "Reviews content and provides feedback."}"#,
        ),
        ("persona_nested_array", r#"["Reviews content"]"#),
        ("persona_nested_long", long_object.as_str()),
    ] {
        respond(mode, &envelope(json!({"description": description})));
        let error = suggest_agent_persona(
            &persona_alias(mode),
            "",
            "claude",
            &idle(),
            &EnvironmentSnapshot::capture(),
        )
        .expect_err("encoded JSON is not persona prose");
        assert!(error.contains("plain prose"), "{error}");
        assert_eq!(calls(mode), 1, "a rejected suggestion is not retried");
    }
    respond(
        "persona_prose_json",
        &envelope(json!({"description": "Reviews JSON APIs and their documentation."})),
    );
    assert_eq!(
        suggest_agent_persona(
            &persona_alias("persona_prose_json"),
            "",
            "claude",
            &idle(),
            &EnvironmentSnapshot::capture()
        )
        .unwrap(),
        "Reviews JSON APIs and their documentation."
    );
}

#[test]
fn malformed_persona_output_is_rejected() {
    respond("persona_garbage", &json!("not the expected envelope"));

    let error = suggest_agent_persona(
        &persona_alias("persona_garbage"),
        "draft",
        "claude",
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect_err("malformed output must not be offered");

    assert!(
        error.contains("malformed") || error.contains("unexpected shape"),
        "got {error:?}"
    );
}

/// The two workers answer different questions; neither answer may be rendered
/// as the other, and a persona never reaches the summary cache.
#[test]
fn a_persona_and_a_status_summary_are_never_swapped() {
    respond(
        "persona_mixed",
        &envelope(json!({"origin_description": "Editing the parser.", "peers": []})),
    );
    let error = suggest_agent_persona(
        &persona_alias("persona_mixed"),
        "",
        "claude",
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect_err("a status summary is not a persona");
    assert!(error.contains("no persona description"), "got {error:?}");

    respond(
        "statusmixed",
        &envelope(json!({"description": "Researches options."})),
    );
    let error = summarize_agent_status(
        &snapshot("statusmixed"),
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect_err("a persona is not a status summary");
    assert!(error.contains("no origin description"), "got {error:?}");
}

#[test]
fn an_unsupported_persona_host_is_reported_rather_than_substituted() {
    let error = suggest_agent_persona(
        &persona_alias("persona_host"),
        "draft",
        "bash",
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect_err("a shell pane has no agent CLI");

    assert!(error.contains("unavailable for this host"), "got {error:?}");
    assert_eq!(calls("persona_host"), 0, "no other provider may be used");
}

#[test]
fn a_request_with_nothing_to_describe_never_spawns_a_worker() {
    fixture();

    let error = suggest_agent_persona(
        "   ",
        "  ",
        "claude",
        &idle(),
        &EnvironmentSnapshot::capture(),
    )
    .expect_err("there is nothing to describe");

    assert!(
        error.contains("needs a name or a description"),
        "got {error:?}"
    );
}
