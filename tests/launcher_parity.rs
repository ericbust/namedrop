#[path = "support/environment.rs"]
mod environment;
use environment::CommandHome;

use std::{fs, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn run_script_matches_base_and_uses_state_runtime_without_bin() {
    assert_eq!(
        include_bytes!("../scripts/run.sh").as_slice(),
        include_bytes!("fixtures/run-base17.sh").as_slice()
    );
    let root = std::env::temp_dir().join(format!("namedrop-run-base-{}", std::process::id()));
    fs::create_dir_all(root.join("scripts")).unwrap();
    fs::create_dir_all(root.join("state/runtime")).unwrap();
    fs::write(
        root.join("scripts/run.sh"),
        include_bytes!("../scripts/run.sh"),
    )
    .unwrap();
    let launcher = root.join("state/runtime/namedrop");
    fs::write(&launcher, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n").unwrap();
    fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700)).unwrap();
    let output = Command::new("/bin/sh")
        .arg(root.join("scripts/run.sh"))
        .arg("--version")
        .isolated_env(root.join("home"))
        .env("PATH", "/usr/bin:/bin")
        .env("HERDR_PANE_ID", "pane")
        .env("HERDR_PLUGIN_STATE_DIR", root.join("state"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"--herdr-pane\npane\n--version\n");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn generated_tui_launcher_preserves_json_parse_errors() {
    let root = std::env::temp_dir().join(format!("namedrop-launcher-json-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let host = root.join("herdr");
    fs::write(&host, r#"#!/bin/sh
case "$1 $2" in
  'worktree list') printf '%s\n' '{"result":{"worktrees":[{"path":"/repo","open_workspace_id":"workspace"}]}}' ;;
  'pane list') printf '%s\n' '{"result":{"panes":[]}}' ;;
  'server reload-config') printf '%s\n' '{}' ;;
  *) exit 97 ;;
esac
"#).unwrap();
    fs::set_permissions(&host, fs::Permissions::from_mode(0o700)).unwrap();
    let activated = Command::new(env!("CARGO_BIN_EXE_namedrop"))
        .isolated_env(root.join("home"))
        .env("PATH", "/usr/bin:/bin")
        .env("HERDR_BIN_PATH", &host)
        .env("SHELL", "/bin/bash")
        .arg("--plugin-state")
        .arg(root.join("state"))
        .args([
            "--herdr-workspace",
            "workspace",
            "--herdr-socket",
            "socket",
            "activate",
            "--input-json",
            "{\"confirmed\":true}",
        ])
        .output()
        .unwrap();
    assert!(
        activated.status.success(),
        "{} {}",
        String::from_utf8_lossy(&activated.stdout),
        String::from_utf8_lossy(&activated.stderr)
    );
    let output = Command::new(root.join("home/.local/bin/namedrop"))
        .isolated_env(root.join("home"))
        .env("PATH", "/usr/bin:/bin")
        .args(["--json", "send", "--bogus"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(64));
    let body: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("launcher must preserve JSON parse errors");
    assert!(body.to_string().contains("invalid_request"));
    fs::remove_dir_all(root).unwrap();
}
