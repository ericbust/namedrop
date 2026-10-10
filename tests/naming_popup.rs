use serde_json::Value;
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

/// Stands in for the host CLI a persona suggestion runs. It records exactly
/// what reached the model, answers with a numbered description so a superseded
/// answer is told apart from the one that replaced it, and can be made slow or
/// made to fail. Nothing here contacts a provider.
const FAKE_HOST: &str = r#"#!/bin/sh
dir="$(dirname "$0")"
payload="$(cat)"
count=$(( $(cat "$dir/calls" 2>/dev/null || echo 0) + 1 ))
printf '%s' "$count" > "$dir/calls"
{
  printf 'CALL %s ARGS<%s>\n' "$count" "$*"
  printf 'HERDR_PANE_ID<%s>\n' "${HERDR_PANE_ID-unset}"
  printf 'STDIN<%s>\n' "$payload"
} >> "$dir/worker-input"
: > "$dir/started-$count"
# Live pane detection, so a test can time an origin change to land after the
# persona step rather than before the popup opens.
if [ -n "$TEST_ORIGIN_CHANGE" ]; then : > "$TEST_DETECTED"; fi
delay="$(cat "$dir/delay-$count" 2>/dev/null || echo 0)"
if [ "$delay" != 0 ]; then sleep "$delay"; fi
if [ -f "$dir/fail" ]; then printf 'the provider is unavailable\n' >&2; exit 3; fi
: > "$dir/finished-$count"
printf '{"description":"Suggested persona %s"}\n' "$count"
"#;

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "namedrop-name-popup-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let host = root.join("herdr");
        fs::write(&host, r#"#!/bin/sh
if [ "$1 $2" = "worktree list" ]; then
  printf '%s\n' '{"result":{"worktrees":[{"path":"/repo","open_workspace_id":"w1"}]}}'
elif [ "$1 $2" = "pane list" ]; then
  if [ "$TEST_AGENT" = 1 ]; then agent="\"agent\":\"${TEST_HOST:-codex}\","; else agent=''; fi
  if [ -n "$TEST_AGENT_SESSION" ]; then agent="$agent\"agent_session\":{\"value\":\"$TEST_AGENT_SESSION\"},"; fi
  # Live detection reads process-info last, so a pane list after it is the popup
  # rechecking its origin with the human's type answer already in hand.
  terminal=t1
  if [ -n "$TEST_ORIGIN_CHANGE" ] && [ -f "$TEST_DETECTED" ]; then
    if [ "$TEST_ORIGIN_CHANGE" = drop ]; then
      printf '%s\n' '{"result":{"panes":[]}}'
      exit 0
    fi
    terminal=t9
  fi
  if [ "$TEST_SECOND_PANE" = 1 ]; then second=',{"pane_id":"p2","terminal_id":"t2","tab_id":"tab1","workspace_id":"w1","agent_status":"idle"}'; else second=''; fi
  printf '%s\n' "{\"result\":{\"panes\":[{\"pane_id\":\"p1\",\"terminal_id\":\"$terminal\",\"tab_id\":\"tab1\",\"workspace_id\":\"w1\",$agent\"agent_status\":\"idle\"}$second]}}"
elif [ "$1 $2" = "pane process-info" ]; then
  # Only the target pane's detection is the one a test times; the popup's own
  # inherited pane is verified through the same call.
  if [ "$4" = p1 ]; then : > "$TEST_DETECTED"; fi
  # The test process is an ancestor of every pane shell this fake describes.
  if [ -f "$TEST_BUSY" ]; then group=$((TEST_SHELL_PID + 1)); else group=$TEST_SHELL_PID; fi
  printf '%s\n' "{\"result\":{\"process_info\":{\"shell_pid\":$TEST_SHELL_PID,\"foreground_process_group_id\":$group,\"foreground_processes\":[{\"pid\":$PPID}]}}}"
elif [ "$1 $2" = "pane run" ]; then
  printf '%s\n' "$@" > "$TEST_RUN"
  HERDR_PANE_ID="$3" /bin/sh -c "exec $4" > "$TEST_RUN_OUTPUT" 2>&1 || exit $?
  printf '%s\n' '{}'
elif [ "$1 $2" = "agent prompt" ]; then
  printf '%s\n' "$@" > "$TEST_PROMPT"
  printf '%s\n' '{}'
elif [ "$1 $2" = "plugin pane" ]; then
  for arg in "$@"; do
    case "$arg" in --workspace|--target-pane) echo 'overlay and popup plugin panes target the active pane' >&2; exit 1;; esac
  done
  printf '%s\n' "$@" > "$TEST_ARGS"
  printf '%s\n' '{"result":{"pane_id":"popup"}}'
else printf '%s\n' '{}'; fi
"#).unwrap();
        fs::set_permissions(host, fs::Permissions::from_mode(0o700)).unwrap();
        // Both supported hosts are replaced, so no test in this suite can
        // reach a real provider even if one is installed on the machine.
        for name in ["claude", "codex"] {
            let script = root.join(name);
            fs::write(&script, FAKE_HOST).unwrap();
            fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self(root)
    }

    /// How many suggestions the fake host CLI was asked for.
    fn calls(&self) -> u32 {
        fs::read_to_string(self.0.join("calls"))
            .unwrap_or_default()
            .trim()
            .parse()
            .unwrap_or(0)
    }

    fn worker_input(&self) -> String {
        fs::read_to_string(self.0.join("worker-input")).unwrap_or_default()
    }

    fn persona(&self) -> Value {
        self.addresses()[0]["persona"].clone()
    }
    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_namedrop"));
        cmd.env("HERDR_BIN_PATH", self.0.join("herdr"))
            .env("HERDR_PLUGIN_STATE_DIR", self.0.join("state"))
            .env("HOME", self.0.join("home"))
            .env("CLAUDE_CONFIG_DIR", self.0.join("home").join(".claude"))
            .env("CODEX_HOME", self.0.join("home").join(".codex"))
            .env_remove("XDG_CONFIG_HOME")
            .env("HERDR_SOCKET_PATH", "test-socket")
            .env("HERDR_WORKSPACE_ID", "popup-workspace")
            .env("HERDR_PANE_ID", "popup-pane")
            .env("TEST_SHELL_PID", std::process::id().to_string())
            .env("NAMEDROP_TARGET_WORKSPACE", "w1")
            .env("NAMEDROP_TARGET_PANE", "p1")
            .env("NAMEDROP_TARGET_TERMINAL", "t1")
            .env("NAMEDROP_TARGET_SESSION", "session-1")
            .env("TEST_ARGS", self.0.join("args"))
            .env("TEST_BUSY", self.0.join("busy"))
            .env("TEST_PROMPT", self.0.join("prompt"))
            .env("TEST_RUN", self.0.join("run"))
            .env("TEST_RUN_OUTPUT", self.0.join("run-output"))
            .env("TEST_DETECTED", self.0.join("detected"))
            .env("NAMEDROP_AGENT_STATUS_CLAUDE_BIN", self.0.join("claude"))
            .env("NAMEDROP_AGENT_STATUS_CODEX_BIN", self.0.join("codex"))
            // Codex is contained by sandbox-exec. Pointing that at a path that
            // does not exist makes a Codex-hosted suggestion fail at once
            // rather than reach anything real.
            .env(
                "NAMEDROP_AGENT_STATUS_SANDBOX_BIN",
                self.0.join("no-sandbox"),
            );
        cmd
    }
    fn run(&self, input: &str, agent: bool) -> Output {
        self.spawn(
            "name",
            input,
            &[("TEST_AGENT", if agent { "1" } else { "0" })],
        )
    }
    fn manage(&self, input: &str) -> Output {
        self.spawn("manage", input, &[("TEST_AGENT", "0")])
    }
    /// Names the pane from a Claude host, so the persona step asks the fake
    /// CLI for a suggestion instead of failing for want of a supported host.
    fn run_on_claude(&self, input: &str) -> Output {
        self.spawn(
            "name",
            input,
            &[("TEST_AGENT", "1"), ("TEST_HOST", "claude")],
        )
    }
    fn manage_on_claude(&self, input: &str) -> Output {
        self.spawn(
            "manage",
            input,
            &[("TEST_AGENT", "1"), ("TEST_HOST", "claude")],
        )
    }
    /// Replaces (`swap`) or removes (`drop`) the invoking pane once live
    /// detection has run, so the change lands after the type prompt.
    fn run_origin_change(&self, input: &str, change: &str) -> Output {
        self.spawn(
            "name",
            input,
            &[("TEST_AGENT", "0"), ("TEST_ORIGIN_CHANGE", change)],
        )
    }
    fn spawn(&self, mode: &str, input: &str, env: &[(&str, &str)]) -> Output {
        let mut command = self.command();
        command.args(["ui", "run", mode]);
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }
    fn addresses(&self) -> Value {
        let out = self
            .command()
            .args(["--herdr-workspace", "w1", "address", "list"])
            .output()
            .unwrap();
        serde_json::from_slice::<Value>(&out.stdout).unwrap()["data"].clone()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn naming_shortcut_has_a_real_popup_entrypoint() {
    let manifest = fs::read_to_string("herdr-plugin.toml").unwrap();
    for expected in [
        "id = \"name-agent\"",
        "\"ui\", \"launch\", \"name\"",
        "[[panes]]\nid = \"naming\"",
        "placement = \"popup\"",
        "\"ui\", \"run\"",
    ] {
        assert!(manifest.contains(expected), "missing {expected}");
    }
}

#[test]
fn status_launch_preserves_origin_and_never_prompts_it() {
    let f = Fixture::new();
    let output = f
        .command()
        .args([
            "--herdr-workspace",
            "w1",
            "--herdr-pane",
            "p1",
            "ui",
            "launch",
            "status",
        ])
        .env("TEST_AGENT", "1")
        .env("TEST_AGENT_SESSION", "origin-session")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let args = fs::read_to_string(f.0.join("args")).unwrap();
    for expected in [
        "--entrypoint\nagent-status",
        "--placement\npopup",
        "NAMEDROP_TARGET_WORKSPACE=w1",
        "NAMEDROP_TARGET_PANE=p1",
        "NAMEDROP_TARGET_TERMINAL=t1",
        "NAMEDROP_TARGET_SESSION=origin-session",
    ] {
        assert!(args.contains(expected), "missing {expected}: {args}");
    }
    assert!(!f.0.join("prompt").exists());
    assert!(!f.0.join("run").exists());
    assert!(!f.0.join("state/namedrop.sqlite3").exists());
}

#[test]
fn doctor_shortcut_opens_a_popup_and_background_checks_stay_noninteractive() {
    let manifest = fs::read_to_string("herdr-plugin.toml")
        .unwrap()
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    let actions = manifest["actions"].as_array_of_tables().unwrap();
    let f = Fixture::new();
    let action = actions
        .iter()
        .find(|a| a["id"].as_str() == Some("doctor"))
        .unwrap();
    let args = action["command"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect::<Vec<_>>();
    let out = f
        .command()
        .args(["--herdr-workspace", "w1", "--herdr-pane", "p1"])
        .args(&args[2..])
        .output()
        .unwrap();
    assert!(
        f.0.join("args").exists(),
        "Doctor produced no floating panel: {out:?}"
    );
    assert!(out.status.success(), "{out:?}");
    let opened = fs::read_to_string(f.0.join("args")).unwrap();
    assert!(opened.contains("--entrypoint\ndoctor"), "{opened}");
    assert!(opened.contains("NAMEDROP_TARGET_WORKSPACE=w1"), "{opened}");
    assert!(!f.0.join("state/namedrop.sqlite3").exists());
    let check = actions
        .iter()
        .find(|a| a["id"].as_str() == Some("check"))
        .unwrap();
    let args = check["command"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect::<Vec<_>>();
    let out = f
        .command()
        .args(["--herdr-workspace", "w1", "--herdr-pane", "p1"])
        .args(&args[2..])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["operation"], "doctor");
    assert_eq!(result["data"]["status"], "needs_attention");
}

#[test]
fn doctor_popup_keeps_diagnostics_visible_and_refresh_is_read_only() {
    let f = Fixture::new();
    let out = f.spawn("doctor", "Rr><\u{1b}", &[]);
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    // Every page, including each refresh, ends in its controls.
    assert!(text.matches("Enter or Esc closes").count() >= 3, "{text}");
    assert!(text.contains("Namedrop needs attention"), "{text}");
    assert!(text.contains("Enter or Esc closes"), "{text}");
    assert!(!text.contains("popup-workspace"), "{text}");
    assert!(!f.0.join("state/namedrop.sqlite3").exists());
    assert!(!f.0.join("run").exists());
}

#[test]
fn doctor_popup_pages_long_guidance_without_hiding_the_controls() {
    let f = Fixture::new();
    let mut state = f.0.clone();
    for _ in 0..4 {
        state.push("long-path-".repeat(20));
    }
    // Arrow keys page as well as < and >. An arrow starts with the Esc byte,
    // which on its own used to close the popup.
    let out = f.spawn(
        "doctor",
        "\u{1b}[C\u{1b}[D><\n",
        &[("HERDR_PLUGIN_STATE_DIR", state.to_str().unwrap())],
    );
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.matches("Page 2/").count() >= 2, "{text}");
    assert!(text.matches("Page 1/").count() >= 3, "{text}");
    assert!(
        text.replace('\n', "").contains("'w1'"),
        "repair command's final target is missing: {text}"
    );
    assert!(text.lines().all(|line| line.len() <= 70), "{text}");
    assert!(!state.exists());
}

#[test]
fn doctor_popup_asks_the_invoking_agent_and_leaves_the_issues_open() {
    let f = Fixture::new();
    assert!(f.run("fixer\n\n\n", true).status.success());
    // Opening Doctor pins the Agent Session the popup will be allowed to ask.
    let launched = f
        .command()
        .args(["--herdr-workspace", "w1", "--herdr-pane", "p1"])
        .args(["ui", "launch", "doctor"])
        .env("TEST_AGENT", "1")
        .env("TEST_AGENT_SESSION", "session-1")
        .output()
        .unwrap();
    assert!(launched.status.success(), "{launched:?}");
    let opened = fs::read_to_string(f.0.join("args")).unwrap();
    assert!(
        opened.contains("NAMEDROP_TARGET_SESSION=session-1"),
        "{opened}"
    );
    // Paging, refresh and close must never ask for help.
    let quiet = f.spawn(
        "doctor",
        "r><\u{1b}",
        &[("TEST_AGENT", "1"), ("TEST_AGENT_SESSION", "session-1")],
    );
    assert!(quiet.status.success(), "{quiet:?}");
    let text = String::from_utf8_lossy(&quiet.stdout);
    assert!(text.contains("A Ask agent to help"), "{text}");
    assert!(!text.contains("Asked @"), "{text}");
    assert!(
        !f.0.join("prompt").exists(),
        "refresh or close asked an agent"
    );

    let out = f.spawn(
        "doctor",
        "A\n",
        &[("TEST_AGENT", "1"), ("TEST_AGENT_SESSION", "session-1")],
    );
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Herdr accepted the prompt."), "{text}");
    assert!(text.contains("not the agent acting"), "{text}");
    assert!(text.contains("no issue is fixed yet."), "{text}");
    assert!(
        text.contains("Go back to the pane you opened Doctor from"),
        "{text}"
    );
    assert!(text.contains("steps it needs from you."), "{text}");
    // The prompt reached the invoking pane itself, not merely some named agent.
    let prompt = fs::read_to_string(f.0.join("prompt")).unwrap();
    let arguments = prompt.lines().collect::<Vec<_>>();
    assert_eq!(arguments[0..3], ["agent", "prompt", "p1"], "{prompt}");
    assert!(
        arguments[3].contains("Namedrop Doctor assistance request for this exact pane p1"),
        "{prompt}"
    );
}

#[test]
fn doctor_popup_asks_an_unnamed_agent_pane_without_inventing_a_mention() {
    let f = Fixture::new();
    let out = f.spawn(
        "doctor",
        "A\n",
        &[("TEST_AGENT", "1"), ("TEST_AGENT_SESSION", "session-1")],
    );
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Herdr accepted the prompt."), "{text}");
    assert!(text.contains("no issue is fixed yet."), "{text}");
    // The pane has no Namedrop name, so the popup must not print one.
    assert!(!text.contains('@'), "{text}");
    let prompt = fs::read_to_string(f.0.join("prompt")).unwrap();
    let arguments = prompt.lines().collect::<Vec<_>>();
    assert_eq!(arguments[0..3], ["agent", "prompt", "p1"], "{prompt}");
}

#[test]
fn doctor_popup_keeps_diagnostics_usable_when_no_agent_can_be_asked() {
    let f = Fixture::new();
    let out = f.spawn("doctor", "A\u{1b}", &[]);
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("The invoking pane is not a supported agent."),
        "{text}"
    );
    assert!(!text.contains("Asked @"), "{text}");
    assert!(!f.0.join("prompt").exists(), "a refusal still prompted");
    // A pane that swapped its agent session while Doctor stayed open.
    let stale = f.spawn(
        "doctor",
        "A\u{1b}",
        &[("TEST_AGENT", "1"), ("TEST_AGENT_SESSION", "session-2")],
    );
    assert!(stale.status.success(), "{stale:?}");
    let text = String::from_utf8_lossy(&stale.stdout);
    assert!(
        text.contains("The pane changed session; nothing was sent."),
        "{text}"
    );
    assert!(
        !f.0.join("prompt").exists(),
        "a stale session still prompted"
    );
    // The popup stays open on its diagnostics instead of closing on the error.
    assert!(
        text.matches("Namedrop needs attention").count() >= 2,
        "{text}"
    );
    assert!(text.lines().all(|line| line.len() <= 70), "{text}");
}

#[test]
fn doctor_popup_shows_unavailable_state_without_modifying_it() {
    let f = Fixture::new();
    fs::create_dir_all(f.0.join("state")).unwrap();
    let state = f.0.join("state/namedrop.sqlite3");
    fs::write(&state, "not a database").unwrap();
    let out = f.spawn("doctor", "\n", &[]);
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("could not complete its health check"),
        "{text}"
    );
    assert!(text.contains("Enter or Esc closes"), "{text}");
    assert_eq!(fs::read_to_string(state).unwrap(), "not a database");
}

#[test]
fn management_shortcut_opens_the_manager_with_the_original_pane_context() {
    let manifest = fs::read_to_string("herdr-plugin.toml")
        .unwrap()
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    let action = manifest["actions"]
        .as_array_of_tables()
        .unwrap()
        .iter()
        .find(|action| action["id"].as_str() == Some("manage-addresses"))
        .expect("Shift+M still points to a missing plugin action");
    let command = action["command"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        command,
        ["sh", "./scripts/run.sh", "ui", "launch", "manage"]
    );
    let f = Fixture::new();
    let out = f
        .command()
        .args(["--herdr-workspace", "w1", "--herdr-pane", "p1"])
        .args(&command[2..])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let args = fs::read_to_string(f.0.join("args")).unwrap();
    assert!(args.contains("--entrypoint\nmanagement"), "{args}");
    assert!(args.contains("NAMEDROP_TARGET_WORKSPACE=w1"), "{args}");
    assert!(args.contains("NAMEDROP_TARGET_PANE=p1"), "{args}");
}

#[test]
fn management_renames_and_toggles_delivery_without_replacing_the_identity() {
    let f = Fixture::new();
    assert!(f.run("build\n\n", false).status.success());
    let original = f.addresses()[0].clone();
    let out = f.manage("\nrchecks\n\nd");
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Manage names"), "{text}");
    assert!(text.contains("@build"), "{text}");
    assert!(text.contains("@checks"), "{text}");
    let changed = f.addresses()[0].clone();
    assert_eq!(changed["id"], original["id"]);
    assert_eq!(changed["pane_id"], "p1");
    assert_eq!(changed["pane_type"], "terminal");
    assert_eq!(changed["alias"], "checks");
    assert_eq!(changed["collaboration_disabled"], true);
    assert!(f.manage("\ne").status.success());
    assert_eq!(f.addresses()[0]["collaboration_disabled"], false);
}

#[test]
fn management_offers_naming_in_a_fresh_space_without_activating_on_open() {
    let f = Fixture::new();
    let out = f.manage("\u{1b}");
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("N Name this pane"));
    let list = f
        .command()
        .args(["--herdr-workspace", "w1", "address", "list"])
        .output()
        .unwrap();
    assert!(
        !list.status.success(),
        "opening activated the space: {list:?}"
    );
    let out = f.manage("Nfirst\n\n");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(f.addresses()[0]["alias"], "first");
    assert_eq!(f.addresses()[0]["pane_id"], "p1");
}

#[test]
fn management_detaches_and_reattaches_to_the_invoking_pane() {
    let f = Fixture::new();
    assert!(f.run("build\n\n", false).status.success());
    let original = f.addresses()[0]["id"].clone();
    assert!(f.manage("\nuy\n").status.success());
    assert!(f.addresses()[0]["pane_id"].is_null());
    let out = f.manage("\nby\n");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(f.addresses()[0]["id"], original);
    assert_eq!(f.addresses()[0]["pane_id"], "p1");
}

#[test]
fn management_removes_only_the_selected_name_after_confirmation() {
    let f = Fixture::new();
    assert!(f.run("build\n\n", false).status.success());
    for input in ["\u{1b}", "\nr\u{1b}", "\nxn\n", "\nx\n"] {
        let out = f.manage(input);
        assert!(out.status.success(), "{input:?}: {out:?}");
        assert_eq!(f.addresses()[0]["alias"], "build");
    }
    let out = f.manage("\nxy\n");
    assert!(out.status.success(), "{out:?}");
    assert!(f.addresses().as_array().unwrap().is_empty());
    assert!(
        !f.0.join("args").exists(),
        "manager tried to open another host pane"
    );
}

// A message never goes to its own author, so terminal work for @build is sent
// from a second named pane.
fn name_sender(f: &Fixture) {
    let named = f
        .command()
        .args([
            "--herdr-workspace",
            "w1",
            "--herdr-pane",
            "p2",
            "name",
            "@sender",
            "--type",
            "agent",
        ])
        .env("TEST_SECOND_PANE", "1")
        .output()
        .unwrap();
    assert!(named.status.success(), "{named:?}");
}

#[test]
fn management_refuses_removal_with_pending_work() {
    let f = Fixture::new();
    assert!(f.run("build\n\n", false).status.success());
    let paused = f
        .command()
        .args(["--herdr-workspace", "w1", "space", "pause"])
        .output()
        .unwrap();
    assert!(paused.status.success(), "{paused:?}");
    name_sender(&f);
    let out = f
        .command()
        .args([
            "--herdr-workspace",
            "w1",
            "--herdr-pane",
            "p2",
            "send",
            "--input-json",
            r#"{"source":"@build printf hello","request_id":"pending-for-manager"}"#,
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let out = f.manage("\nxy\n");
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("requires every queued delivery to be terminal"),
        "{text}"
    );
    assert_eq!(f.addresses()[0]["alias"], "build");
    assert_eq!(f.addresses()[0]["pane_id"], "p1");
}

#[test]
fn management_enable_retries_busy_terminal_work_while_the_menu_stays_open() {
    let f = Fixture::new();
    assert!(f.run("build\n\n", false).status.success());
    let operation = |args: &[&str]| {
        let out = f
            .command()
            .args(["--herdr-workspace", "w1", "--herdr-pane", "p1"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    };
    operation(&["space", "pause"]);
    name_sender(&f);
    let sent = f
        .command()
        .args([
            "--herdr-workspace",
            "w1",
            "--herdr-pane",
            "p2",
            "send",
            "--input-json",
        ])
        .arg(r#"{"source":"@build printf done","request_id":"manager-enable-retry"}"#)
        .output()
        .unwrap();
    assert!(sent.status.success(), "{sent:?}");
    assert!(f.manage("\nd").status.success());
    operation(&["space", "resume"]);
    fs::write(f.0.join("busy"), "busy").unwrap();
    let mut child = f
        .command()
        .args(["ui", "run", "manage"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    input.write_all(b"\ne").unwrap();
    let mut displayed = Vec::new();
    let mut byte = [0];
    while displayed
        .windows("↑/↓ Select · Enter Manage".len())
        .filter(|part| *part == "↑/↓ Select · Enter Manage".as_bytes())
        .count()
        < 2
    {
        assert_eq!(child.stdout.as_mut().unwrap().read(&mut byte).unwrap(), 1);
        displayed.push(byte[0]);
    }
    fs::remove_file(f.0.join("busy")).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    while !f.0.join("run").exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let dispatched_while_open = f.0.join("run").exists();
    // Closing this fixture emits no Herdr events. Only the manager's watcher
    // can notice the shell becoming idle without further human input.
    input.write_all(b"\x04").unwrap();
    drop(input);
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(
        dispatched_while_open,
        "enabling left the queued command without a watcher"
    );
    assert!(
        fs::read_to_string(f.0.join("run"))
            .unwrap()
            .starts_with("pane\nrun\np1\n")
    );
    // Dispatch starts before the command and its watcher finish writing state.
    // Join that work before Fixture removes its directory (CI can run slower).
    let directory = fs::read_dir(f.0.join("state/agent-status-reconciliation"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.join("worker.lock"))
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if lock.try_lock().is_ok() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "test worker did not finish: {}",
            fs::read_to_string(f.0.join("run-output")).unwrap_or_default()
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(fs::read_dir(directory).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("pending-")
    }));
    let output = fs::read_to_string(f.0.join("run-output")).unwrap();
    assert!(output.contains("[Namedrop: exit 0]"), "{output}");
}

#[test]
fn management_pages_through_names_and_edits_the_selected_identity() {
    let f = Fixture::new();
    for index in 1..=9 {
        let name = if index == 9 {
            "long-name".repeat(10)
        } else {
            format!("name-{index}")
        };
        assert!(f.run(&format!("{name}\n\n"), false).status.success());
        let addresses = f.addresses();
        let address = addresses
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["pane_id"] == "p1")
            .unwrap();
        let out = f
            .command()
            .args([
                "--herdr-workspace",
                "w1",
                "address",
                "unbind",
                "--input-json",
                &serde_json::json!({"address_id":address["id"]}).to_string(),
            ])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
    }
    let selected_id = f.addresses()[8]["id"].clone();
    let keys = format!("{}\u{1b}[A\u{1b}OB\nrrenamed\n", "\u{1b}[B".repeat(10));
    let out = f.manage(&keys);
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("Page 2/2"));
    for line in String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|line| line.starts_with("  @") || line.starts_with("> @"))
    {
        assert!(line.len() <= 70, "menu row wraps: {line}");
    }
    let addresses = f.addresses();
    assert_eq!(addresses.as_array().unwrap().len(), 9);
    assert_eq!(
        addresses
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["id"] == selected_id)
            .unwrap()["alias"],
        "renamed"
    );
}

#[test]
fn management_rechecks_the_physical_pane_after_the_menu_choice() {
    let f = Fixture::new();
    assert!(f.run("build\n\n", false).status.success());
    fs::remove_file(f.0.join("detected")).unwrap();
    let mut child = f
        .command()
        .args(["ui", "run", "manage"])
        .env("TEST_ORIGIN_CHANGE", "swap")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    input.write_all(b"\n").unwrap();
    let mut displayed = Vec::new();
    let mut byte = [0];
    while !displayed.ends_with(b"Enter/Esc Back\n\n") {
        assert_eq!(child.stdout.as_mut().unwrap().read(&mut byte).unwrap(), 1);
        displayed.push(byte[0]);
    }
    fs::write(f.0.join("detected"), "changed").unwrap();
    input.write_all(b"rghost\n").unwrap();
    drop(input);
    let out = child.wait_with_output().unwrap();
    assert!(!out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("invoking pane changed"));
    assert_eq!(f.addresses()[0]["alias"], "build");
}

#[test]
fn launch_pins_invoking_pane_not_popup_and_forwards_state() {
    let f = Fixture::new();
    let out = f
        .command()
        .args([
            "--herdr-workspace",
            "w1",
            "--herdr-pane",
            "p1",
            "ui",
            "launch",
            "name",
        ])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let args = fs::read_to_string(f.0.join("args")).unwrap();
    for expected in [
        "--placement\npopup",
        "--entrypoint\nnaming",
        "NAMEDROP_TARGET_WORKSPACE=w1",
        "NAMEDROP_TARGET_PANE=p1",
        "NAMEDROP_TARGET_TERMINAL=t1",
        "HERDR_PLUGIN_STATE_DIR=",
        "--focus",
    ] {
        assert!(args.contains(expected), "missing {expected}: {args}");
    }
    // Herdr popups always target the active pane; explicit --target-pane is rejected.
    assert!(!args.contains("--target-pane"));
    assert!(!args.contains("--workspace\n"));
}

#[test]
fn shell_defaults_terminal_and_rename_retains_type_when_agent_starts() {
    let f = Fixture::new();
    let out = f.run("build\n\n", false);
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("[terminal]"));
    let addresses = f.addresses();
    assert_eq!(addresses[0]["alias"], "build");
    assert_eq!(addresses[0]["pane_type"], "terminal");
    assert_eq!(addresses[0]["pane_id"], "p1");
    assert!(f.run("logs\n\n\n", true).status.success());
    assert_eq!(f.addresses()[0]["pane_type"], "terminal");
}

#[test]
fn agent_defaults_agent_and_type_can_be_overridden_independent_of_name() {
    let f = Fixture::new();
    let out = f.run("review\n\n\n", true);
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("[agent]"));
    assert_eq!(f.addresses()[0]["pane_type"], "agent");
    assert!(f.run("review\nterminal\n", false).status.success());
    assert_eq!(f.addresses()[0]["pane_type"], "terminal");
}

#[test]
fn cancelling_either_field_does_not_create_state() {
    for input in ["\u{1b}", "build\n\u{1b}", "build\n"] {
        let f = Fixture::new();
        let out = f.run(input, false);
        assert!(out.status.success(), "{out:?}");
        assert!(!f.0.join("state/namedrop.sqlite3").exists());
    }
}

#[test]
fn origin_replaced_while_choosing_a_type_saves_nothing() {
    // The pre-prompt check cannot see a pane that is replaced or closed while the
    // human is still answering, so the popup rechecks its origin before saving.
    // "Nothing was named" belongs to that later check alone, and reaching the type
    // prompt proves the earlier check had already passed.
    for (change, expected) in [
        ("swap", "Nothing was named"),
        ("drop", "The invoking pane has closed"),
    ] {
        let f = Fixture::new();
        let out = f.run_origin_change("ghost\n\n", change);
        assert!(!out.status.success(), "{change}: {out:?}");
        let prompted = String::from_utf8_lossy(&out.stdout);
        assert!(prompted.contains("Pane type:"), "{change}: {prompted}");
        let reported = String::from_utf8_lossy(&out.stderr);
        assert!(reported.contains(expected), "{change}: {reported}");
        assert!(!f.0.join("state/namedrop.sqlite3").exists(), "{change}");
    }
}

#[test]
fn replaced_target_is_rejected_without_naming_popup() {
    let f = Fixture::new();
    let out = f
        .command()
        .args(["ui", "run"])
        .env("NAMEDROP_TARGET_TERMINAL", "replaced")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!f.0.join("state/namedrop.sqlite3").exists());
}

#[test]
fn naming_an_agent_offers_a_suggestion_and_saves_only_what_is_accepted() {
    let f = Fixture::new();
    // Name, keep the default type, take the suggestion, save it.
    let out = f.run_on_claude("reviewer\n\nA\n");
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Persona for @reviewer"), "{text}");
    assert!(text.contains("Suggested persona 1"), "{text}");
    assert!(text.contains("A Use suggestion"), "{text}");
    assert!(text.contains("Enter Save"), "{text}");
    assert!(text.contains("G Generic"), "{text}");
    assert!(text.contains("S Suggest again"), "{text}");
    assert_eq!(f.persona(), "Suggested persona 1");
    assert_eq!(f.addresses()[0]["pane_type"], "agent");

    // Exactly one suggestion was asked for, and the name and the draft are the
    // whole of what reached it: no identifiers, no pane text, no workspace.
    assert_eq!(f.calls(), 1, "{}", f.worker_input());
    let worker = f.worker_input();
    assert!(
        worker.contains(r#"STDIN<{"draft":"","name":"reviewer"}>"#),
        "{worker}"
    );
    assert!(worker.contains("HERDR_PANE_ID<unset>"), "{worker}");
}

#[test]
fn a_named_terminal_is_never_asked_for_a_persona() {
    let f = Fixture::new();
    let out = f.spawn(
        "name",
        "build\nterminal\n",
        &[("TEST_AGENT", "1"), ("TEST_HOST", "claude")],
    );
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!text.contains("Persona for"), "{text}");
    assert_eq!(f.addresses()[0]["pane_type"], "terminal");
    assert_eq!(f.persona(), Value::Null);
    assert_eq!(f.calls(), 0, "a terminal asked for a suggestion");
    // The manager offers no persona editor for it either.
    let out = f.manage_on_claude("\n\u{1b}\u{1b}");
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!text.contains("P Edit persona"), "{text}");
    assert!(text.contains("Persona: —"), "{text}");
}

#[test]
fn custom_text_generic_and_cancellation_each_leave_the_state_they_promise() {
    let f = Fixture::new();
    // Typed text wins over the suggestion that was offered beside it.
    let out = f.run_on_claude("reviewer\n\nEHunts flaky tests.\n\n");
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Hunts flaky tests."), "{text}");
    assert!(text.contains("Typing · Enter Done"), "{text}");
    assert_eq!(f.persona(), "Hunts flaky tests.");

    // Cancelling the editor leaves the saved persona exactly as it was.
    let out = f.manage_on_claude("\np\u{1b}\u{1b}\u{1b}");
    assert!(out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("The saved persona is unchanged"),
        "{out:?}"
    );
    assert_eq!(f.persona(), "Hunts flaky tests.");

    // Generic is a choice the user makes, and it clears what was saved.
    let out = f.manage_on_claude("\npG\u{1b}\u{1b}");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(f.persona(), Value::Null);
    // Generic is spelled out rather than shown as an empty field.
    let out = f.manage_on_claude("\u{1b}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("Persona: Generic"),
        "{out:?}"
    );
}

#[test]
fn naming_an_existing_agent_shows_and_suggests_from_its_saved_persona() {
    let f = Fixture::new();
    assert!(
        f.run_on_claude("reviewer\n\nEHunts flaky tests.\n\n")
            .status
            .success()
    );
    let original = f.addresses()[0]["id"].clone();
    let out = f.manage_on_claude("nchecks\n\n\n\u{1b}");
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!text.contains("Generic. No specialization"), "{text}");
    assert!(text.contains("\nPersona\n  Hunts flaky tests.\n"), "{text}");
    assert!(
        f.worker_input()
            .contains(r#"STDIN<{"draft":"Hunts flaky tests.","name":"checks"}>"#)
    );
    assert_eq!(f.persona(), "Hunts flaky tests.");
    assert_eq!(f.addresses()[0]["id"], original);
    assert_eq!(f.addresses()[0]["alias"], "checks");
}

#[test]
fn renaming_an_agent_keeps_its_persona_until_a_replacement_is_accepted() {
    let f = Fixture::new();
    assert!(f.run_on_claude("reviewer\n\nA\n").status.success());
    assert_eq!(f.persona(), "Suggested persona 1");
    let original = f.addresses()[0]["id"].clone();

    // Rename, then leave the persona editor without accepting anything.
    let out = f.manage_on_claude("\nrchecks\n\u{1b}\u{1b}");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(f.addresses()[0]["alias"], "checks");
    assert_eq!(f.addresses()[0]["id"], original);
    assert_eq!(
        f.persona(),
        "Suggested persona 1",
        "a rename replaced a persona"
    );
    // The replacement offered was generated for the new name.
    assert!(
        f.worker_input()
            .contains(r#"STDIN<{"draft":"Suggested persona 1","name":"checks"}>"#),
        "the replacement was not generated from the new name and saved text: {}",
        f.worker_input()
    );

    // Renaming again and accepting the replacement does change it.
    let out = f.manage_on_claude("\nrreviews\nA\n\u{1b}");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(f.addresses()[0]["alias"], "reviews");
    assert_eq!(f.persona(), "Suggested persona 3");
}

#[test]
fn a_failed_or_unsupported_suggestion_never_blocks_a_save() {
    let f = Fixture::new();
    fs::write(f.0.join("fail"), "").unwrap();
    let out = f.run_on_claude("reviewer\n\nEWrites release notes.\n\n");
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("No suggestion"), "{text}");
    assert!(text.contains("the provider is unavailable"), "{text}");
    assert_eq!(f.persona(), "Writes release notes.");
    fs::remove_file(f.0.join("fail")).unwrap();

    // A manager opened from a pane with no supported host gets no suggestions,
    // and every other part of the editor still works.
    // The field starts from what is saved, so typing extends it rather than
    // silently replacing the text the user already accepted.
    let out = f.manage("\npE Reviews releases too.\n\n\u{1b}");
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("persona suggestions are unavailable for this host"),
        "{text}"
    );
    assert_eq!(f.persona(), "Writes release notes. Reviews releases too.");
    assert_eq!(f.calls(), 1, "an unsupported host ran a worker");
}

#[test]
fn the_origin_is_rechecked_after_the_persona_step() {
    let f = Fixture::new();
    let out = f.spawn(
        "name",
        "ghost\n\nA\n",
        &[
            ("TEST_AGENT", "1"),
            ("TEST_HOST", "claude"),
            ("TEST_ORIGIN_CHANGE", "swap"),
        ],
    );
    assert!(!out.status.success(), "{out:?}");
    // The editor ran to the end; only the check that follows it refused.
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("Persona for @ghost"),
        "{out:?}"
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Nothing was named"),
        "{out:?}"
    );
    assert!(!f.0.join("state/namedrop.sqlite3").exists());
}

#[test]
fn cancelling_the_persona_step_names_nothing_and_generic_still_names() {
    // Naming promises Esc saves nothing, and the persona step is part of it.
    for input in [
        "reviewer\n\n\u{1b}",
        "reviewer\n\n",
        "reviewer\n\nEdraft\n\u{1b}\u{1b}",
    ] {
        let f = Fixture::new();
        let out = f.run_on_claude(input);
        assert!(out.status.success(), "{input:?}: {out:?}");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("Persona for @reviewer"),
            "{input:?}: {out:?}"
        );
        assert!(
            !f.0.join("state/namedrop.sqlite3").exists(),
            "{input:?} named the pane anyway"
        );
    }
    // Generic is a decision, not a cancellation: the pane is named, Generic.
    let f = Fixture::new();
    let out = f.run_on_claude("reviewer\n\nG");
    assert!(out.status.success(), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("Named @reviewer as agent."),
        "{out:?}"
    );
    assert_eq!(f.persona(), Value::Null);
    // So is an explicit Save of the empty field it opened with.
    let f = Fixture::new();
    assert!(f.run_on_claude("keeper\n\n\n").status.success());
    assert_eq!(f.addresses()[0]["alias"], "keeper");
    assert_eq!(f.persona(), Value::Null);
}

#[test]
fn the_persona_limit_counts_characters_and_never_splits_one() {
    let f = Fixture::new();
    // 999 ASCII characters then a two-byte one: the limit must be reached by
    // the character, not by the first of its bytes.
    let full = format!("{}é", "x".repeat(999));
    assert_eq!(full.chars().count(), 1_000);
    assert_eq!(full.len(), 1_001, "the last character must be multi-byte");
    let out = f.run_on_claude(&format!("reviewer\n\nE{full}\n\n"));
    assert!(out.status.success(), "{out:?}");
    assert_eq!(f.persona(), Value::String(full.clone()));

    // One ASCII character more is refused, and refusing it leaves the saved
    // text whole rather than half a character short or half a character long.
    let out = f.manage_on_claude("\npEz\n\n\u{1b}");
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("A persona is at most 1000 characters."),
        "{text}"
    );
    assert!(text.contains("The persona is unchanged."), "{text}");
    assert_eq!(f.persona(), Value::String(full.clone()));

    // An over-limit multi-byte character is refused whole. Taking its
    // continuation byte alone would leave the draft invalid, which Save would
    // then have to refuse, losing text the user never changed.
    let out = f.manage_on_claude("\npEé\n\n\u{1b}");
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("A persona is at most 1000 characters."),
        "{text}"
    );
    assert!(
        !text.contains("not valid UTF-8"),
        "a refused character corrupted the draft: {text}"
    );
    assert!(text.contains("The persona is unchanged."), "{text}");
    assert_eq!(f.persona(), Value::String(full));

    // The field is still usable afterwards: backspace removes whole
    // characters, and a multi-byte character can be typed once there is room.
    let out = f.manage_on_claude("\npE\u{7f}\u{7f}aé\n\n\u{1b}");
    assert!(out.status.success(), "{out:?}");
    let expected = format!("{}aé", "x".repeat(998));
    assert_eq!(expected.chars().count(), 1_000);
    assert_eq!(f.persona(), Value::String(expected));
}

#[test]
fn an_inert_persona_on_a_retyped_terminal_is_hidden_but_preserved() {
    let f = Fixture::new();
    assert!(f.run_on_claude("reviewer\n\nA\n").status.success());
    assert_eq!(f.persona(), "Suggested persona 1");
    // Only explicit user direction changes a persona, so retyping the name as
    // a terminal leaves it stored and inert rather than clearing it.
    assert!(f.run("reviewer\nterminal\n", false).status.success());
    assert_eq!(f.addresses()[0]["pane_type"], "terminal");
    assert_eq!(
        f.persona(),
        "Suggested persona 1",
        "retyping rewrote the saved persona"
    );

    let out = f.manage("\n\u{1b}\u{1b}");
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        !text.contains("Suggested persona 1"),
        "an inert persona was displayed: {text}"
    );
    assert!(text.contains("Persona: —"), "{text}");
    assert!(!text.contains("P Edit persona"), "{text}");
    assert_eq!(f.calls(), 1, "the manager asked for a suggestion");
}

#[test]
fn the_persona_editor_stays_live_while_a_suggestion_runs_in_a_real_terminal() {
    let f = Fixture::new();
    assert!(f.run_on_claude("reviewer\n\nA\n").status.success());
    assert_eq!(f.persona(), "Suggested persona 1");
    // The suggestion the editor opens with is slow, its replacement is not,
    // and the one after that is slow again so a save can outrun it.
    fs::write(f.0.join("delay-2"), "8").unwrap();
    fs::write(f.0.join("delay-4"), "8").unwrap();
    let command = f.command();
    let out = Command::new("/usr/bin/python3")
        .envs(command.get_envs().filter_map(|(key, value)| value.map(|v| (key, v))))
        .env("TEST_AGENT", "1")
        .env("TEST_HOST", "claude")
        // Colour splits a hint into runs; this test reads words, not colour.
        .env("NO_COLOR", "1")
        .args(["-c", r#"
import os, pty, select, subprocess, sys, time
master, slave = pty.openpty()
child = subprocess.Popen([sys.argv[1], 'ui', 'run', 'manage'], stdin=slave, stdout=slave, stderr=slave)
fixture = os.path.dirname(os.environ['NAMEDROP_AGENT_STATUS_CLAUDE_BIN'])
transcript = bytearray()
def pump(seconds):
    if select.select([master], [], [], seconds)[0]:
        chunk = os.read(master, 65536)
        assert chunk, bytes(transcript)
        transcript.extend(chunk)
def expect(text, timeout=20):
    deadline = time.monotonic() + timeout
    while text.encode() not in transcript:
        assert time.monotonic() < deadline, (text, bytes(transcript))
        pump(0.1)
def started(call, timeout=20):
    deadline = time.monotonic() + timeout
    while not os.path.exists(os.path.join(fixture, 'started-%d' % call)):
        assert time.monotonic() < deadline, (call, bytes(transcript))
        pump(0.1)
try:
    expect('Enter Manage')
    os.write(master, b'\r')
    expect('P Edit persona')
    os.write(master, b'p')
    # The editor paints and starts counting before any answer exists.
    expect('Suggesting… 0s')
    expect('Suggested persona 1')
    # Typing is accepted and echoed while that request is still running.
    os.write(master, b'E')
    expect('Typing')
    for byte in b' Handles migrations.':
        os.write(master, bytes([byte]))
        time.sleep(0.01)
    expect('Suggested persona 1 Handles migrations.')
    # Elapsed time keeps moving while the text is being edited.
    expect('Suggesting… 2s')
    os.write(master, b'\r')
    # Asking again supersedes the request in flight.
    os.write(master, b'S')
    expect('Suggested persona 3')
    # The superseded answer is never shown, and the typed text is untouched.
    assert b'Suggested persona 2' not in transcript, bytes(transcript)
    assert b'Suggested persona 1 Handles migrations.' in transcript, bytes(transcript)
    # Saving never waits on inference: ask again, then save while it runs.
    os.write(master, b'S')
    started(4)
    os.write(master, b'\r')
    expect('Persona: Suggested persona 1 Handles migrations.')
    os.write(master, b'\x1b')
    assert child.wait(timeout=5) == 0, bytes(transcript)
finally:
    if child.poll() is None:
        child.kill()
        child.wait()
    os.close(master)
    os.close(slave)
"#])
        .arg(env!("CARGO_BIN_EXE_namedrop"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // The superseded worker was started and then reaped: it never ran to the
    // end, and no later call was left behind either.
    assert!(f.0.join("started-2").exists(), "the slow worker never ran");
    assert!(
        !f.0.join("finished-2").exists(),
        "the superseded worker was left running"
    );
    assert!(f.0.join("finished-3").exists(), "the replacement never ran");
    // The save did not wait for the suggestion that was still running, and
    // leaving the editor reaped it.
    assert!(f.0.join("started-4").exists());
    assert!(
        !f.0.join("finished-4").exists(),
        "saving waited for the suggestion in flight"
    );
    assert_eq!(f.calls(), 4);
    assert_eq!(f.persona(), "Suggested persona 1 Handles migrations.");
}

#[test]
fn management_single_keys_and_escape_work_in_a_real_terminal() {
    let f = Fixture::new();
    assert!(f.run("temporary\n\n", false).status.success());
    assert!(f.manage("\nxy\n").status.success());
    let command = f.command();
    let out = Command::new("/usr/bin/python3")
        .envs(command.get_envs().filter_map(|(key, value)| value.map(|v| (key, v))))
        // Colour splits a hint into runs; this test reads words, not colour.
        .env("NO_COLOR", "1")
        .args(["-c", r#"
import os, pty, select, subprocess, sys, termios, time
master, slave = pty.openpty()
saved = termios.tcgetattr(slave)
child = subprocess.Popen([sys.argv[1], 'ui', 'run', 'manage'], stdin=slave, stdout=slave, stderr=slave)
transcript = bytearray()
def expect(text):
    received = bytearray()
    deadline = time.monotonic() + 5
    while text.encode() not in received:
        assert time.monotonic() < deadline, (text, bytes(transcript))
        if select.select([master], [], [], 0.1)[0]:
            chunk = os.read(master, 65536)
            assert chunk, bytes(transcript)
            received.extend(chunk)
            transcript.extend(chunk)
    return received
try:
    empty = expect('Esc Close')
    assert b'No named panes yet. Press N' in empty
    assert b'Select a name' not in empty
    os.write(master, b'R')
    expect('Esc Close')
    os.write(master, b'N')
    expect('Mention name: @')
    os.write(master, b'first\n')
    expect('Pane type:')
    os.write(master, b'\n')
    expect('Enter Manage')
    # Send one complete key: a sleep between bytes can exceed the Esc timeout
    # on a busy runner, turning the simulated arrow into a standalone Esc.
    os.write(master, b'\x1b[A')
    expect('Enter Manage')
    os.write(master, b'\r')
    expect('Enter/Esc Back')
    os.write(master, b'R')
    expect('New name: @')
    os.write(master, b'updated\n')
    expect('Enter Manage')
    os.write(master, b'\r')
    expect('Enter/Esc Back')
    os.write(master, b'\x1b')
    expect('Enter Manage')
    os.write(master, b'\x1b')
    assert child.wait(timeout=3) == 0, bytes(transcript)
    restored = termios.tcgetattr(slave)
    # macOS may set the transient retype-pending flag after stty restores input.
    saved[3] &= ~termios.PENDIN
    restored[3] &= ~termios.PENDIN
    assert restored == saved, ('terminal settings were not restored', saved, restored)
finally:
    if child.poll() is None:
        child.kill()
        child.wait()
    os.close(master)
    os.close(slave)
"#])
        .arg(env!("CARGO_BIN_EXE_namedrop"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(f.addresses()[0]["alias"], "updated");
}

impl Fixture {
    fn tour(&self, input: &str) -> Output {
        self.spawn("tour", input, &[("TEST_AGENT", "0")])
    }
    fn doctor(&self, input: &str) -> Output {
        self.spawn("doctor", input, &[("TEST_AGENT", "0")])
    }
    fn marker(&self) -> std::path::PathBuf {
        self.0
            .join("state/tours-shown")
            .join(namedrop::tour::CURRENT_ID)
    }
    /// Activates the space without naming anything, so Manage can list an
    /// empty set of names instead of reporting an inactive space.
    fn activate(&self) {
        let out = self
            .command()
            .args(["--herdr-workspace", "w1", "--herdr-pane", "p1", "activate"])
            .args(["--input-json", r#"{"confirmed":true}"#])
            .env("TEST_AGENT", "0")
            .output()
            .unwrap();
        let result: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(result["data"]["activated"], true, "{out:?}");
    }
}

/// The page numbers the popup drew, in the order it drew them.
fn drawn_pages(text: &str) -> Vec<usize> {
    text.lines()
        .filter_map(|line| line.strip_prefix("Page "))
        .map(|rest| rest.split('/').next().unwrap().parse().unwrap())
        .collect()
}

#[test]
fn tour_popup_pages_and_marks_the_presentation_shown() {
    let presentation = namedrop::tour::current().unwrap();
    let count = presentation.pages.len();
    assert!(count >= 2, "the tour needs pages to page through");
    let f = Fixture::new();
    assert!(!f.marker().exists());
    // Forward with every next key, one press past the last page, back with
    // every previous key, then close with Q.
    let mut keys = String::new();
    let forward = [">", " ", "\u{1b}[C", "\u{1b}[B"];
    for step in 0..count {
        keys.push_str(forward[step % forward.len()]);
    }
    let back = ["<", "\u{1b}[D", "\u{1b}[A"];
    for step in 0..count - 1 {
        keys.push_str(back[step % back.len()]);
    }
    keys.push('q');
    let out = f.tour(&keys);
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);

    let expected = (1..=count)
        .chain(std::iter::once(count))
        .chain((1..count).rev())
        .collect::<Vec<_>>();
    assert_eq!(drawn_pages(&text), expected, "{text}");
    // Each page's title leads its page, and the pages come in file order.
    let mut from = 0;
    for page in &presentation.pages {
        let at = text[from..]
            .find(&format!("{}\n\n", page.title))
            .unwrap_or_else(|| panic!("page {:?} is missing or out of order: {text}", page.title));
        from += at + page.title.len();
        let body = namedrop::tour::wrapped_body(page, namedrop::tour::BODY_WIDTH);
        assert!(
            text[from..].starts_with(&format!("\n\n{}\n", body[0])),
            "{text}"
        );
    }
    for (index, page) in presentation.pages.iter().enumerate() {
        let mut controls = format!("Page {}/{count} · ←/→ Page", index + 1);
        if page.action == Some(namedrop::tour::PageAction::Name) {
            controls.push_str(" · N Name this pane");
        }
        assert!(
            text.lines().any(|line| line == controls),
            "missing {controls:?}: {text}"
        );
    }
    assert_eq!(
        text.matches("Enter or Esc closes").count(),
        expected.len(),
        "{text}"
    );
    // Piped output is plain text: no colour, cursor or clearing sequences.
    assert!(!text.contains('\u{1b}'), "{text:?}");
    assert!(text.lines().all(|line| line.len() <= 70), "{text}");
    assert!(f.marker().is_file(), "the tour was not recorded as shown");
    // Opening it again still works and leaves the marker in place.
    let again = f.tour("\u{1b}");
    assert!(again.status.success(), "{again:?}");
    assert!(String::from_utf8_lossy(&again.stdout).contains(&presentation.pages[0].title));
    assert!(f.marker().is_file());
    // The tour never touches naming state on its own.
    assert!(!f.0.join("state/namedrop.sqlite3").exists());
}

#[test]
fn tour_popup_n_opens_the_name_prompt() {
    let presentation = namedrop::tour::current().unwrap();
    let action = presentation
        .pages
        .iter()
        .position(|page| page.action == Some(namedrop::tour::PageAction::Name))
        .expect("the tour offers naming");
    let plain = presentation
        .pages
        .iter()
        .position(|page| page.action.is_none())
        .expect("the tour has a page without an action");
    let f = Fixture::new();
    // N does nothing on a page without the action, then names the pane from
    // the page that offers it and returns to that same page.
    let mut keys = ">".repeat(plain);
    keys.push('n');
    keys.push_str(&"<".repeat(plain));
    keys.push_str(&">".repeat(action));
    keys.push_str("nguide\n\nq");
    let out = f.tour(&keys);
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(text.matches("Mention name: @").count(), 1, "{text}");
    let after = &text[text
        .find("Named @guide as terminal.")
        .expect("no confirmation")..];
    // The page is drawn again with what was named at its top.
    let redrawn = format!(
        "Named @guide as terminal.\n\n{}\n",
        presentation.pages[action].title
    );
    assert!(after.contains(&redrawn), "{text}");
    assert_eq!(drawn_pages(after), [action + 1], "{text}");
    let addresses = f.addresses();
    assert_eq!(addresses.as_array().unwrap().len(), 1, "{addresses}");
    assert_eq!(addresses[0]["alias"], "guide");
    assert_eq!(addresses[0]["pane_id"], "p1");
    assert_eq!(addresses[0]["pane_type"], "terminal");
    // Esc in the name prompt names nothing and returns to the page.
    let f = Fixture::new();
    let mut keys = ">".repeat(action);
    keys.push_str("n\u{1b}q");
    let out = f.tour(&keys);
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Mention name: @"), "{text}");
    assert_eq!(drawn_pages(&text).last(), Some(&(action + 1)), "{text}");
    assert!(!text.contains("Named "), "{text}");
}

#[test]
fn reset_tour_clears_the_shown_marker() {
    let f = Fixture::new();
    assert!(f.tour("q").status.success());
    assert!(f.marker().is_file());
    let reset = || {
        let out = f.command().args(["ui", "reset-tour"]).output().unwrap();
        assert!(out.status.success(), "{out:?}");
        serde_json::from_slice::<Value>(&out.stdout).unwrap()
    };
    let first = reset();
    assert_eq!(first["operation"], "ui.reset-tour", "{first}");
    assert_eq!(
        first["data"],
        serde_json::json!({"presentation_id": namedrop::tour::CURRENT_ID, "cleared": true})
    );
    assert!(!f.marker().exists());
    let second = reset();
    assert_eq!(
        second["data"],
        serde_json::json!({"presentation_id": namedrop::tour::CURRENT_ID, "cleared": false})
    );
    assert!(!f.marker().exists());
}

#[test]
fn manage_and_doctor_offer_the_tour_when_nothing_is_named() {
    let first = namedrop::tour::current().unwrap().pages[0].title.clone();
    let f = Fixture::new();
    f.activate();
    // T pages the tour inline, and closing it returns to the list.
    let out = f.manage("tq\u{1b}");
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("No named panes yet. Press N to name this pane.\nPress T for the tour."),
        "{text}"
    );
    let tour = text
        .find(&format!("{first}\n\n"))
        .expect("T showed no tour");
    assert!(text[tour..].contains("Manage names"), "{text}");
    assert!(
        !f.marker().exists(),
        "only the tour popup records it as shown"
    );

    // Doctor always has an issue while nothing is named: no agent, or an
    // agent without a name. It offers the tour below the report.
    let out = f.doctor("tq\n");
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Namedrop needs attention"), "{text}");
    assert!(text.contains("\n\nPress T for the tour.\n\n"), "{text}");
    let tour = text
        .find(&format!("{first}\n\n"))
        .expect("T showed no tour");
    assert!(text[tour..].contains("Namedrop needs attention"), "{text}");
    // A fresh space is offered it too, and Doctor still creates no state.
    let fresh = Fixture::new();
    let out = fresh.doctor("tq\n");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("Press T for the tour."), "{text}");
    assert!(text.contains(&first), "{text}");
    assert!(!fresh.0.join("state").exists(), "{:?}", fresh.0);

    // Once a pane is named, neither offers it and T does nothing.
    assert!(f.run("build\n\n", false).status.success());
    for out in [f.manage("t\u{1b}"), f.doctor("t\n")] {
        assert!(out.status.success(), "{out:?}");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(!text.contains("Press T for the tour."), "{text}");
        assert!(!text.contains(&first), "{text}");
    }
}

#[test]
fn launch_tour_opens_the_tour_popup() {
    let manifest = fs::read_to_string("herdr-plugin.toml")
        .unwrap()
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    let action = manifest["actions"]
        .as_array_of_tables()
        .unwrap()
        .iter()
        .find(|action| action["id"].as_str() == Some("tour"))
        .expect("the tour has no plugin action");
    let command = action["command"]
        .as_array()
        .unwrap()
        .iter()
        .map(|arg| arg.as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(command, ["sh", "./scripts/run.sh", "ui", "launch", "tour"]);
    let f = Fixture::new();
    let out = f
        .command()
        .args(["--herdr-workspace", "w1", "--herdr-pane", "p1"])
        .args(&command[2..])
        .env("TEST_AGENT", "1")
        .env("TEST_AGENT_SESSION", "origin-session")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["operation"], "ui.launch");
    assert_eq!(
        result["data"],
        serde_json::json!({"result":{"pane_id":"popup"}})
    );
    let args = fs::read_to_string(f.0.join("args")).unwrap();
    let state = format!("HERDR_PLUGIN_STATE_DIR={}", f.0.join("state").display());
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        [
            "plugin",
            "pane",
            "open",
            "--plugin",
            "namedrop",
            "--entrypoint",
            "tour",
            "--placement",
            "popup",
            "--width",
            "72",
            "--height",
            "24",
            "--env",
            "NAMEDROP_TARGET_WORKSPACE=w1",
            "--env",
            "NAMEDROP_TARGET_PANE=p1",
            "--env",
            "NAMEDROP_TARGET_TERMINAL=t1",
            "--env",
            "NAMEDROP_TARGET_SESSION=origin-session",
            "--env",
            state.as_str(),
            "--env",
            "HERDR_SOCKET_PATH=test-socket",
            "--focus",
        ]
    );
    // Launching opens the popup only; the popup itself records the tour.
    assert!(!f.marker().exists());
    assert!(!f.0.join("state/namedrop.sqlite3").exists());
}
