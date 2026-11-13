#[path = "support/binary.rs"]
mod binary;

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Output, Stdio},
};

struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("nd-binding-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(path.join("home")).unwrap();
        fs::create_dir_all(path.join("state")).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The context hook as a Herdr pane runs it, with a Herdr server that is not
/// there, so the hook falls silent as it does today.
fn hook(root: &Path, event: &str, runtime: Option<&str>) -> Output {
    let mut command = binary::namedrop();
    command
        .args(["hook", event, "--state-directory"])
        .arg(root.join("state"))
        .args(
            runtime
                .map(|runtime| ["--runtime", runtime])
                .into_iter()
                .flatten(),
        )
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("home/.config"))
        .env("HERDR_SOCKET_PATH", root.join("no-herdr.sock"))
        .env("HERDR_WORKSPACE_ID", "w1")
        .env("HERDR_PANE_ID", "w1:p1")
        .env("HERDR_BIN_PATH", root.join("no-herdr"))
        .env_remove("NAMEDROP_STATE_DIR")
        .env_remove("HERDR_PLUGIN_STATE_DIR")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().unwrap();
    let name = if event == "session-start" {
        "SessionStart"
    } else {
        "UserPromptSubmit"
    };
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            format!(r#"{{"hook_event_name":"{name}","source":"startup","session_id":"s1","prompt":"hello"}}"#)
                .as_bytes(),
        )
        .unwrap();
    child.wait_with_output().unwrap()
}

/// Only a Rex block binds a session. A Herdr pane's hooks take the wrapper's
/// `--runtime`, answer as they answer without it, and write no binding.
#[test]
fn a_herdr_home_ignores_the_runtime_and_writes_no_binding() {
    let scratch = Scratch::new("herdr");
    for event in ["session-start", "user-prompt-submit"] {
        let with = hook(&scratch.0, event, Some("claude-code"));
        let without = hook(&scratch.0, event, None);
        assert!(with.status.success(), "{with:?}");
        assert_eq!(
            (with.status, &with.stdout),
            (without.status, &without.stdout),
            "{event}"
        );
        assert!(
            !scratch.0.join("state/rex").exists(),
            "{event} wrote under the state directory's rex/"
        );
    }
}
