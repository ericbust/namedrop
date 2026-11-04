#[path = "support/cleanup.rs"]
mod cleanup;

use namedrop::{AgentRuntime, install_integration_package};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::Duration,
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "namedrop stop {}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("home")).unwrap();
        fs::create_dir_all(root.join("config")).unwrap();
        Self(root)
    }

    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.0.join("home"))
            .env("XDG_CONFIG_HOME", self.0.join("config"));
        command
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        cleanup::remove_fixture(&self.0).unwrap();
    }
}

#[test]
fn both_installed_stop_hooks_return_silently_while_the_worker_is_still_running() {
    for runtime in [AgentRuntime::ClaudeCode, AgentRuntime::Codex] {
        let fixture = Fixture::new();
        let binary = fixture.0.join("fake namedrop");
        fs::write(&binary, "#!/bin/sh\ncat > capture\nprintf '%s\\n' \"$@\" > args\ntouch ready\ni=0\nwhile [ ! -e release ] && [ \"$i\" -lt 200 ]; do sleep 0.01; i=$((i+1)); done\necho forbidden-output\necho forbidden-diagnostic >&2\ntouch finished\n").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        let state = fixture.0.join("state");
        let package =
            install_integration_package(&fixture.0.join("home"), runtime, &binary, &state).unwrap();
        let manifest: Value =
            serde_json::from_slice(&fs::read(package.join("hooks/hooks.json")).unwrap()).unwrap();
        assert!(manifest["hooks"]["SessionStart"].is_array());
        assert!(manifest["hooks"]["UserPromptSubmit"].is_array());
        let launch = manifest["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        let payload = json!({"hook_event_name":"Stop", "session_id":"test", "last_assistant_message":"Which one?"}).to_string();
        let mut child = fixture
            .command("/bin/sh")
            .args(["-c", launch])
            .current_dir(&fixture.0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
        assert!(
            !fixture.0.join("finished").exists(),
            "Stop waited for its worker"
        );
        for _ in 0..200 {
            if fixture.0.join("ready").exists() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            fs::read_to_string(fixture.0.join("capture")).unwrap(),
            payload
        );
        let args = fs::read_to_string(fixture.0.join("args")).unwrap();
        assert_eq!(
            args,
            format!("hook\nstop\n--state-directory\n{}\n", state.display())
        );
        fs::write(fixture.0.join("release"), "").unwrap();
        for _ in 0..200 {
            if fixture.0.join("finished").exists() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(fixture.0.join("finished").exists());
    }
}

#[test]
fn stop_command_is_silent_and_does_not_create_state_when_unavailable() {
    let fixture = Fixture::new();
    for input in [
        "not json".to_owned(),
        json!({"hook_event_name":"Stop", "session_id":"test", "last_assistant_message":"hello"})
            .to_string(),
    ] {
        let state = fixture.0.join("missing-state");
        let mut child = fixture
            .command(env!("CARGO_BIN_EXE_namedrop"))
            .args(["hook", "stop", "--state-directory"])
            .arg(&state)
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
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
        assert!(!state.exists());
    }
}
