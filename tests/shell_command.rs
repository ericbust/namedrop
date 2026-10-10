use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

struct Fixture(PathBuf);
impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "namedrop-shell-{}-{label} with 'quotes'",
            std::process::id()
        ));
        fs::create_dir_all(root.join("home")).unwrap();
        let host = root.join("herdr");
        fs::write(&host, r#"#!/bin/sh
case "$1 $2" in
  'worktree list') printf '%s\n' '{"result":{"worktrees":[{"path":"/repo","open_workspace_id":"w1"}]}}';;
  'pane list') printf '%s\n' '{"result":{"panes":[{"pane_id":"p1","terminal_id":"t1","tab_id":"tab1","workspace_id":"w1","agent_status":"unknown"}]}}';;
  'pane process-info') printf '%s\n' '{"result":{"process_info":{"pane_id":"p1","shell_pid":SHELL_PID,"foreground_process_group_id":SHELL_PID,"foreground_processes":[{"pid":SHELL_PID,"name":"zsh","argv0":"zsh"}]}}}';;
  'server reload-config') if [ -e "$HOME/reload-fails" ]; then echo 'reload unavailable' >&2; exit 1; fi; printf '%s\n' reloaded >> "$HOME/reloads"; printf '%s\n' '{}';;
  'tab get') printf '%s\n' '{"result":{"label":"Main"}}';;
  'pane report-metadata'|'tab rename') printf '%s\n' '{}';;
  *) printf 'unexpected Herdr call: %s\n' "$*" >&2; exit 1;;
esac
"#.replace("SHELL_PID", &std::process::id().to_string())).unwrap();
        fs::set_permissions(host, fs::Permissions::from_mode(0o700)).unwrap();
        Self(root)
    }
    fn home(&self) -> PathBuf {
        self.0.join("home")
    }
    fn command(&self, executable: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(executable);
        command
            .env("HOME", self.home())
            .env("CLAUDE_CONFIG_DIR", self.home().join(".claude"))
            .env("CODEX_HOME", self.home().join(".codex"))
            .env_remove("XDG_CONFIG_HOME")
            .env("CODEX_HOME", self.home().join(".codex"))
            .env_remove("ZDOTDIR")
            .env_remove("HERDR_CONFIG_PATH")
            .env_remove("HERDR_PLUGIN_STATE_DIR")
            .env("SHELL", "/bin/zsh")
            .env("PATH", "/usr/bin:/bin")
            .env("HERDR_BIN_PATH", self.0.join("herdr"))
            .env("HERDR_SOCKET_PATH", "isolated-socket")
            .env("HERDR_WORKSPACE_ID", "w1")
            .env("HERDR_PANE_ID", "p1");
        command
    }
    fn activate(&self, binary: &std::path::Path) -> std::process::Output {
        let mut command = self.command(binary);
        command
            .arg("--plugin-state")
            .arg(self.0.join("state"))
            .args(["activate", "--input-json", "{\"confirmed\":true}"]);
        // A test that just copied `binary` can exec it while another test's fork still
        // holds the copy's write descriptor until its own exec, which fails with ETXTBSY.
        for _ in 0..100 {
            match command.output() {
                Err(error) if error.raw_os_error() == Some(libc::ETXTBSY) => {
                    std::thread::sleep(std::time::Duration::from_millis(20))
                }
                result => return result.unwrap(),
            }
        }
        command.output().unwrap()
    }
    fn cli(&self, args: &[&str]) -> std::process::Output {
        self.command(env!("CARGO_BIN_EXE_namedrop"))
            .arg("--plugin-state")
            .arg(self.0.join("state"))
            .args(args)
            .output()
            .unwrap()
    }
    fn terminal(&self, args: &[&str]) -> std::process::Output {
        self.command("/usr/bin/python3")
            .args([
                "-c",
                r#"
import errno, os, pty, subprocess, sys
master, slave = pty.openpty()
child = subprocess.Popen(sys.argv[1:], stdin=subprocess.DEVNULL, stdout=slave, stderr=slave)
os.close(slave)
try:
    while True:
        try:
            chunk = os.read(master, 65536)
        except OSError as error:
            if error.errno == errno.EIO: break
            raise
        if not chunk: break
        sys.stdout.buffer.write(chunk)
finally:
    os.close(master)
sys.exit(child.wait(timeout=10))
"#,
            ])
            .arg(env!("CARGO_BIN_EXE_namedrop"))
            .arg("--plugin-state")
            .arg(self.0.join("state"))
            .args(args)
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn success(output: std::process::Output) -> String {
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn activation_makes_the_readme_command_work_in_a_fresh_shell_and_refreshes_its_path() {
    let f = Fixture::new("activation");
    fs::write(
        f.home().join(".zshrc"),
        "# existing settings\nexport KEEP_ME=yes\n",
    )
    .unwrap();
    let binary = std::path::Path::new(env!("CARGO_BIN_EXE_namedrop"));
    success(f.activate(binary));
    let wrapper = f.home().join(".local/bin/namedrop");
    assert!(
        wrapper.is_file(),
        "activation must install the namedrop command"
    );
    let first_rc = fs::read(f.home().join(".zshrc")).unwrap();
    let named = success(
        f.command("/bin/zsh")
            .args(["-ic", "namedrop name @build --type terminal"])
            .output()
            .unwrap(),
    );
    let named: serde_json::Value = serde_json::from_str(&named).unwrap();
    assert_eq!(named["data"]["alias"], "@build");
    assert_eq!(named["data"]["pane_type"], "terminal");
    assert!(f.0.join("state/namedrop.sqlite3").is_file());

    let next = f.0.join("replacement binary");
    fs::copy(binary, &next).unwrap();
    success(f.activate(&next));
    assert!(
        fs::read_to_string(&wrapper)
            .unwrap()
            .contains("replacement binary")
    );
    assert_eq!(fs::read(f.home().join(".zshrc")).unwrap(), first_rc);
    let version = success(
        f.command("/bin/zsh")
            .args(["-ic", "namedrop --version; test \"$KEEP_ME\" = yes"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        version.trim(),
        format!("namedrop {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn activation_preserves_an_unrelated_command_and_shell_configuration() {
    let f = Fixture::new("conflict");
    fs::create_dir_all(f.home().join(".local/bin")).unwrap();
    let launcher = f.home().join(".local/bin/namedrop");
    fs::write(&launcher, "#!/bin/sh\necho user-owned\n").unwrap();
    fs::write(f.home().join(".zshrc"), "# untouched\n").unwrap();
    let result = f.activate(std::path::Path::new(env!("CARGO_BIN_EXE_namedrop")));
    assert!(!result.status.success());
    assert!(
        String::from_utf8(result.stdout)
            .unwrap()
            .contains("not owned by Namedrop")
    );
    assert_eq!(
        fs::read_to_string(launcher).unwrap(),
        "#!/bin/sh\necho user-owned\n"
    );
    assert_eq!(
        fs::read_to_string(f.home().join(".zshrc")).unwrap(),
        "# untouched\n"
    );
}

#[test]
fn activation_refuses_symlinked_startup_files_before_installing_the_command() {
    let f = Fixture::new("symlink");
    let original = f.0.join("user configuration");
    fs::write(&original, "# preserved\n").unwrap();
    std::os::unix::fs::symlink(&original, f.home().join(".zshrc")).unwrap();
    let result = f.activate(std::path::Path::new(env!("CARGO_BIN_EXE_namedrop")));
    assert!(!result.status.success());
    assert!(
        String::from_utf8(result.stdout)
            .unwrap()
            .contains("symbolic link")
    );
    assert!(!f.home().join(".local/bin/namedrop").exists());
    assert_eq!(fs::read_to_string(original).unwrap(), "# preserved\n");
}

#[test]
fn activation_reports_partial_installation_as_failure_in_json_and_human_output() {
    let f = Fixture::new("package-conflict");
    let skill = f.home().join(".agents/skills/namedrop");
    fs::create_dir_all(&skill).unwrap();
    fs::write(skill.join("SKILL.md"), "user-owned skill\n").unwrap();
    for human in [false, true] {
        let mut command = f.command(env!("CARGO_BIN_EXE_namedrop"));
        command.arg("--plugin-state").arg(f.0.join("state")).args([
            "activate",
            "--input-json",
            "{\"confirmed\":true}",
        ]);
        if human {
            command.arg("--human");
        } else {
            command.arg("--json");
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        if human {
            let message = String::from_utf8_lossy(&output.stderr);
            assert!(message.contains("integration needs attention"), "{message}");
            assert!(message.contains("State:"), "{message}");
        } else {
            let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(response["data"]["activated"], true);
            assert_eq!(
                response["data"]["integration_packages"]["codex"]["installed"],
                false
            );
        }
    }
    assert_eq!(
        fs::read_to_string(skill.join("SKILL.md")).unwrap(),
        "user-owned skill\n"
    );
}

#[test]
fn activation_supports_bash_login_and_interactive_shells_without_shadowing_profile() {
    let f = Fixture::new("bash");
    fs::write(f.home().join(".profile"), "export KEEP_ME=yes\n").unwrap();
    let activated = f
        .command(env!("CARGO_BIN_EXE_namedrop"))
        .env("SHELL", "/bin/bash")
        .arg("--plugin-state")
        .arg(f.0.join("state"))
        .args(["activate", "--input-json", "{\"confirmed\":true}"])
        .output()
        .unwrap();
    success(activated);
    assert!(!f.home().join(".bash_profile").exists());
    for flag in ["-lc", "-ic"] {
        let output = success(
            f.command("/bin/bash")
                .args([flag, "namedrop --version"])
                .output()
                .unwrap(),
        );
        assert_eq!(
            output.trim(),
            format!("namedrop {}", env!("CARGO_PKG_VERSION"))
        );
    }
    success(
        f.command("/bin/bash")
            .args(["-lc", "test \"$KEEP_ME\" = yes"])
            .output()
            .unwrap(),
    );
}

#[test]
fn activation_honors_zdotdir_inside_the_isolated_home() {
    let f = Fixture::new("zdotdir");
    let zdotdir = f.home().join("zsh config");
    success(
        f.command(env!("CARGO_BIN_EXE_namedrop"))
            .env("ZDOTDIR", &zdotdir)
            .arg("--plugin-state")
            .arg(f.0.join("state"))
            .args(["activate", "--input-json", "{\"confirmed\":true}"])
            .output()
            .unwrap(),
    );
    assert!(!f.home().join(".zshrc").exists());
    let output = success(
        f.command("/bin/zsh")
            .env("ZDOTDIR", zdotdir)
            .args(["-ic", "namedrop --version"])
            .output()
            .unwrap(),
    );
    assert_eq!(
        output.trim(),
        format!("namedrop {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn terminal_commands_are_readable_while_pipes_and_explicit_json_keep_the_protocol() {
    let f = Fixture::new("readable");
    success(f.activate(std::path::Path::new(env!("CARGO_BIN_EXE_namedrop"))));
    let text = success(f.terminal(&["name", "@build", "--type", "terminal"]));
    assert!(text.contains("Named this terminal @build."), "{text}");
    assert!(text.contains("Ready to receive commands."));
    assert!(
        !text.contains("protocol_version") && !text.contains("registered"),
        "{text}"
    );
    for output in [
        f.cli(&["address", "list"]),
        f.terminal(&["address", "list", "--json"]),
    ] {
        let result: serde_json::Value = serde_json::from_str(&success(output)).unwrap();
        assert_eq!(result["protocol_version"], "collab/v1");
        assert_eq!(result["data"][0]["alias"], "build");
    }
    let text = success(f.terminal(&["address", "list"]));
    assert!(text.contains("bound") && !text.contains("empty"), "{text}");
    assert!(
        text.contains("NAME")
            && text.contains("@build")
            && text.contains("terminal")
            && text.contains("p1"),
        "{text}"
    );
    assert!(!text.contains("protocol_version"));
    let text = success(f.cli(&["group", "list", "--human"]));
    assert_eq!(text.trim(), "No saved groups in this space.");
    let description: serde_json::Value =
        serde_json::from_str(&success(f.terminal(&["describe", "send"]))).unwrap();
    assert_eq!(description["protocol_version"], "collab/v1");
}

#[test]
fn human_health_and_errors_preserve_failure_status_and_json_can_be_forced() {
    let f = Fixture::new("health-output");
    success(f.activate(std::path::Path::new(env!("CARGO_BIN_EXE_namedrop"))));
    success(f.cli(&["name", "@build", "--type", "terminal"]));
    assert!(success(f.terminal(&["space", "pause"])).contains("Messages will wait"));
    let report = f.cli(&["doctor", "--human"]);
    assert_eq!(report.status.code(), Some(1));
    assert!(report.stdout.is_empty());
    let text = String::from_utf8(report.stderr).unwrap();
    assert!(
        text.contains("Namedrop needs attention") && text.contains("paused"),
        "{text}"
    );
    let report = f.terminal(&["doctor", "--json"]);
    assert_eq!(report.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&report.stdout).unwrap();
    assert_eq!(report["data"]["status"], "needs_attention");
    assert!(success(f.terminal(&["space", "resume"])).contains("Namedrop resumed"));
    assert!(success(f.terminal(&["doctor"])).contains("checks passed"));
    let invalid = f.cli(&["--human", "not-a-command"]);
    assert_eq!(invalid.status.code(), Some(64));
    assert!(invalid.stdout.is_empty());
    assert!(
        String::from_utf8(invalid.stderr)
            .unwrap()
            .contains("unrecognized subcommand")
    );
    let invalid = f.terminal(&["--json", "not-a-command"]);
    assert_eq!(invalid.status.code(), Some(64));
    let invalid: serde_json::Value = serde_json::from_slice(&invalid.stdout).unwrap();
    assert_eq!(invalid["status"], "rejected");
}

#[test]
fn activation_installs_and_reloads_every_shortcut_without_an_extra_command() {
    let f = Fixture::new("status-shortcut");
    let binary = std::path::Path::new(env!("CARGO_BIN_EXE_namedrop"));
    let first: serde_json::Value = serde_json::from_str(&success(f.activate(binary))).unwrap();
    assert_eq!(first["data"]["shortcuts"]["status"], "installed");
    assert_eq!(first["data"]["shortcuts"]["reloaded"], true);
    assert!(first["data"]["shortcuts"]["next_step"].is_null());
    let path = f.home().join(".config/herdr/config.toml");
    let config = fs::read_to_string(&path).unwrap();
    let parsed = config.parse::<toml_edit::DocumentMut>().unwrap();
    let bound: Vec<_> = parsed["keys"]["command"]
        .as_array_of_tables()
        .unwrap()
        .iter()
        .map(|binding| {
            assert_eq!(binding["type"].as_str(), Some("plugin_action"));
            (
                binding["key"].as_str().unwrap(),
                binding["command"].as_str().unwrap(),
            )
        })
        .collect();
    // Doctor avoids prefix+shift+d, which is Herdr's own close_workspace.
    assert_eq!(
        bound,
        [
            ("prefix+shift+s", "namedrop.agent-status"),
            ("prefix+shift+i", "namedrop.name-agent"),
            ("prefix+shift+m", "namedrop.manage-addresses"),
            ("prefix+shift+h", "namedrop.doctor"),
            ("prefix+shift+o", "namedrop.tour"),
        ]
    );
    let human = success(f.cli(&[
        "--human",
        "activate",
        "--input-json",
        "{\"confirmed\":true}",
    ]));
    for line in [
        "Agent status shortcut: prefix+shift+s",
        "Name this pane shortcut: prefix+shift+i",
        "Manage names shortcut: prefix+shift+m",
        "Doctor shortcut: prefix+shift+h",
        "Tour shortcut: prefix+shift+o",
    ] {
        assert!(human.contains(line), "{human}");
    }
    // Updating uses activation too; repeats do not duplicate or rewrite bindings.
    success(f.activate(binary));
    assert_eq!(fs::read_to_string(&path).unwrap(), config);
    assert_eq!(
        fs::read_to_string(f.home().join("reloads"))
            .unwrap()
            .lines()
            .count(),
        3
    );
    // A previously chosen alternative key is retained and loaded.
    let custom = config.replace("prefix+shift+h", "prefix+shift+d");
    fs::write(&path, &custom).unwrap();
    let report: serde_json::Value = serde_json::from_str(&success(f.activate(binary))).unwrap();
    assert_eq!(report["data"]["shortcuts"]["status"], "already_configured");
    assert_eq!(
        report["data"]["shortcuts"]["bindings"][3]["key"],
        "prefix+shift+d"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), custom);
}

#[test]
fn activation_reports_shortcut_conflicts_and_reload_failures() {
    let f = Fixture::new("status-conflict");
    let path = f.home().join(".config/herdr/config.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conflict =
        "# user key\n[[keys.command]]\nkey='prefix+shift+s'\ntype='shell'\ncommand='echo keep'\n";
    fs::write(&path, conflict).unwrap();
    let human = success(f.cli(&[
        "--human",
        "activate",
        "--input-json",
        "{\"confirmed\":true}",
    ]));
    assert!(
        human.contains("Agent status shortcut needs attention"),
        "{human}"
    );
    assert!(human.contains("already has a binding"), "{human}");
    assert!(human.contains("Doctor shortcut: prefix+shift+h"), "{human}");
    // The taken key keeps its binding; the other shortcuts are still added and loaded.
    let after = fs::read_to_string(&path).unwrap();
    assert!(after.starts_with(conflict), "{after}");
    let parsed = after.parse::<toml_edit::DocumentMut>().unwrap();
    let commands = parsed["keys"]["command"].as_array_of_tables().unwrap();
    assert_eq!(commands.len(), 5);
    assert!(
        commands
            .iter()
            .all(|binding| binding["command"].as_str() != Some("namedrop.agent-status"))
    );
    assert!(f.home().join("reloads").exists());
    fs::write(&path, "# preserve this comment\n").unwrap();
    fs::write(f.home().join("reload-fails"), "").unwrap();
    let report: serde_json::Value = serde_json::from_str(&success(
        f.activate(std::path::Path::new(env!("CARGO_BIN_EXE_namedrop"))),
    ))
    .unwrap();
    assert_eq!(report["data"]["shortcuts"]["reloaded"], false);
    assert!(
        report["data"]["shortcuts"]["error"]
            .as_str()
            .unwrap()
            .contains("reload unavailable")
    );
    assert!(
        fs::read_to_string(&path)
            .unwrap()
            .contains("# preserve this comment")
    );
    fs::remove_file(f.home().join("reload-fails")).unwrap();
    let report: serde_json::Value = serde_json::from_str(&success(
        f.activate(std::path::Path::new(env!("CARGO_BIN_EXE_namedrop"))),
    ))
    .unwrap();
    assert_eq!(report["data"]["shortcuts"]["reloaded"], true);
}

#[test]
fn activation_uses_the_config_override_and_refuses_a_symlink() {
    let f = Fixture::new("status-config-override");
    let config = f.0.join("custom/config.toml");
    let activate = || {
        let mut command = f.command(env!("CARGO_BIN_EXE_namedrop"));
        command
            .env("HERDR_CONFIG_PATH", &config)
            .arg("--plugin-state")
            .arg(f.0.join("state"))
            .args(["activate", "--input-json", "{\"confirmed\":true}"]);
        serde_json::from_str::<serde_json::Value>(&success(command.output().unwrap())).unwrap()
    };
    let report = activate();
    assert_eq!(
        report["data"]["shortcuts"]["path"],
        config.to_str().unwrap()
    );
    assert!(config.exists());
    assert!(!f.home().join(".config/herdr/config.toml").exists());
    let original = f.0.join("original-config.toml");
    fs::rename(&config, &original).unwrap();
    let before = fs::read(&original).unwrap();
    std::os::unix::fs::symlink(&original, &config).unwrap();
    let report = activate();
    assert_eq!(report["data"]["shortcuts"]["status"], "unavailable");
    assert!(
        report["data"]["shortcuts"]["error"]
            .as_str()
            .unwrap()
            .contains("regular file")
    );
    assert_eq!(fs::read(&original).unwrap(), before);
}
