#[path = "support/environment.rs"]
mod environment;
use environment::CommandHome;

use namedrop::*;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

fn pending_update(
    path: &Path,
    before: Option<&str>,
    after: Option<&str>,
    added: &[Value],
) -> Value {
    use sha2::{Digest, Sha256};
    let hash =
        |text: Option<&str>| text.map(|text| namedrop::lower_hex(&Sha256::digest(text.as_bytes())));
    json!({"path":path,"before_hash":hash(before),"after_hash":hash(after),"added":added,"removed":[],"mode":384})
}

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    root: PathBuf,
    home: PathBuf,
    claude: PathBuf,
    ids: Ids,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "namedrop-claude-permissions-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let home = root.join("home");
        fs::create_dir_all(home.join(".claude")).unwrap();
        let claude = home.join(".claude");
        let herdr = root.join("herdr");
        fs::write(&herdr, r#"#!/usr/bin/python3
import json, os, sys
args=sys.argv[1:]
w=os.environ['TEST_WORKSPACE']
if args[:2]==['worktree','list']:
    result={'worktrees':[{'path':'/repo','open_workspace_id':w}]}
elif args[:2]==['pane','list']:
    result={'panes':[{'pane_id':'pane-1','terminal_id':'terminal-1','tab_id':'tab-1','workspace_id':w,'agent':'claude','agent_session':{'value':'session-1'},'agent_status':'idle'}]}
    if os.environ.get('TEST_PEER_PANE'):
        result['panes'].append({'pane_id':'pane-2','terminal_id':'terminal-2','tab_id':'tab-1','workspace_id':w,'agent':'claude','agent_session':{'value':'session-2'},'agent_status':'idle'})
else: result={}
print(json.dumps({'result':result}))
"#).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&herdr, fs::Permissions::from_mode(0o700)).unwrap();
        Self {
            root,
            home,
            claude,
            ids: Ids(AtomicU64::new(0)),
        }
    }
    fn settings(&self) -> PathBuf {
        self.claude.join("settings.json")
    }
    fn command_for(&self, workspace: &str) -> std::process::Command {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_namedrop"));
        command
            .args([
                "--plugin-state",
                self.root.to_str().unwrap(),
                "--herdr-socket",
                "socket-a",
                "--herdr-workspace",
                workspace,
                "--herdr-pane",
                "pane-1",
            ])
            .isolated_home(&self.home)
            .env("CLAUDE_CONFIG_DIR", &self.claude)
            .env("CODEX_HOME", self.home.join(".codex"))
            .env("HERDR_BIN_PATH", self.root.join("herdr"))
            .env("TEST_WORKSPACE", workspace);
        command
    }
    fn run_for(&self, workspace: &str, operation: &str, input: Value) -> Value {
        let output = self
            .command_for(workspace)
            .args([operation, "--input-json", &input.to_string()])
            .output()
            .unwrap();
        let report: Value =
            serde_json::from_slice(&output.stdout).unwrap_or_else(|_| panic!("{output:?}"));
        assert_eq!(report["status"], "ok", "{output:?}");
        report["data"].clone()
    }
    fn doctor(&self) -> HealthReport {
        let output = self
            .command_for("workspace-1")
            .arg("doctor")
            .output()
            .unwrap();
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        serde_json::from_value(report["data"].clone()).unwrap_or_else(|_| panic!("{output:?}"))
    }
    fn runtime(&self) -> LifecycleRuntime<'_, namedrop::TuiHost<&'_ Host>, Time, Ids> {
        LifecycleRuntime {
            state_directory: &self.root,
            home_directory: Some(&self.home),
            agent_config: namedrop::AgentConfigDirectories {
                claude: Some(&self.claude),
                codex: None,
                ..Default::default()
            },
            cwd: Path::new("/repo"),
            workspace_id: None,
            current_pane_id: Some("pane-1"),
            session_socket_id: "socket-a",
            host: namedrop::TuiHost::new(&Host),
            clock: &Time,
            ids: &self.ids,
        }
    }
    fn activate(&self) -> Value {
        self.run_for("workspace-1", "activate", json!({"confirmed":true}))
    }
    fn deactivate(&self) {
        self.run_for("workspace-1", "deactivate", json!({}));
    }
    fn read(&self) -> Value {
        serde_json::from_slice(&fs::read(self.settings()).unwrap()).unwrap()
    }
    fn expected(&self) -> Vec<Value> {
        self.expected_for(Path::new(env!("CARGO_BIN_EXE_namedrop")))
    }
    fn expected_for(&self, executable: &Path) -> Vec<Value> {
        [
            "describe",
            "register",
            "ack",
            "send",
            "reply",
            "complete --no-reply",
            "delivery inbox",
            "snapshot publish",
            "context inspect",
            "context read",
            "handoff status",
            "handoff results",
            "handoff preview",
            "group list",
            "handoff begin",
            "handoff resolve",
            "handoff adopt",
            "address list",
        ]
        .into_iter()
        .map(|operation| {
            json!(format!(
                "Bash('{}' --plugin-state '{}' {operation} *)",
                executable.display(),
                self.root.display()
            ))
        })
        .collect()
    }
    fn bootstrap_rule_for(&self, executable: &Path) -> Value {
        json!(format!(
            "Bash(HERDR_PLUGIN_STATE_DIR='{}' HERDR_SOCKET_PATH=* HERDR_WORKSPACE_ID=* HERDR_PANE_ID=* '{}' register --input-json *)",
            self.root.display(),
            executable.display()
        ))
    }
    fn bootstrap_rule(&self) -> Value {
        self.bootstrap_rule_for(Path::new(env!("CARGO_BIN_EXE_namedrop")))
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
struct Host;
impl HerdrCommandExecutor for Host {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let stdout = match args {
            ["workspace", "list"] => {
                json!({"result":{"workspaces":[{"workspace_id":"workspace-1"}]}})
            }
            ["worktree", "list", ..] => {
                json!({"result":{"worktrees":[{"path":"/repo", "open_workspace_id":"workspace-1"}]}})
            }
            ["pane", "list", ..] => {
                json!({"result":{"panes":[{"pane_id":"pane-1", "terminal_id":"terminal-1", "tab_id":"tab-1", "workspace_id":"workspace-1", "agent":"claude", "agent_session":{"value":"session-1"}, "agent_status":"idle"}]}})
            }
            ["pane", "report-metadata", ..] | ["agent", "prompt", ..] => json!({}),
            _ => return Err(format!("unexpected command: {args:?}")),
        };
        Ok(HerdrCommandOutput {
            stdout: stdout.to_string(),
        })
    }
}
struct Time;
impl Clock for Time {
    fn now_unix_ms(&self) -> u64 {
        42
    }
    fn wait_ms(&self, _: u64) {}
}
struct Ids(AtomicU64);
impl IdGenerator for Ids {
    fn next_id(&self, kind: &str) -> String {
        format!("{kind}-{}", self.0.fetch_add(1, Ordering::Relaxed))
    }
}

#[test]
fn activation_ignores_python_shims_on_path() {
    use std::os::unix::fs::PermissionsExt;

    let f = Fixture::new();
    let python = f.root.join("python3");
    fs::write(
        &python,
        "#!/bin/sh\necho unexpected-python-shim >&2\nexit 97\n",
    )
    .unwrap();
    fs::set_permissions(&python, fs::Permissions::from_mode(0o700)).unwrap();
    let output = f
        .command_for("workspace-1")
        .env("PATH", format!("{}:/usr/bin:/bin", f.root.display()))
        .args(["activate", "--input-json", r#"{"confirmed":true}"#])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "ok", "{report}");
    assert_eq!(f.read()["permissions"]["allow"], json!(f.expected()));
}

#[test]
fn activation_preserves_settings_and_adds_managed_command_permissions() {
    let f = Fixture::new();
    fs::write(f.settings(), r#"{"model":"user-model","permissions":{"deny":["Bash(rm *)"],"allow":["Read","Bash(echo *)",42]},"env":{"USER_KEY":"value"}}"#).unwrap();
    f.activate();
    let settings = f.read();
    assert_eq!(settings["model"], "user-model");
    assert_eq!(settings["env"], json!({"USER_KEY":"value"}));
    assert_eq!(settings["permissions"]["deny"], json!(["Bash(rm *)"]));
    let allow = settings["permissions"]["allow"].as_array().unwrap();
    assert_eq!(
        &allow[..3],
        &[json!("Read"), json!("Bash(echo *)"), json!(42)]
    );
    assert_eq!(&allow[3..], f.expected());
    let skill =
        fs::read_to_string(package_directory(&f.home, AgentRuntime::ClaudeCode).join("SKILL.md"))
            .unwrap();
    assert!(!skill.contains("allowed-tools:"), "{skill}");
    assert!(!skill.contains("retains `allowed-tools`"), "{skill}");
    let contents = fs::read_to_string(f.settings()).unwrap();
    assert!(contents.find("\"model\"").unwrap() < contents.find("\"permissions\"").unwrap());
    let rules = fs::read_to_string(f.home.join(".codex/rules/namedrop.rules")).unwrap();
    let expected: Vec<Value> = rules
        .lines()
        .filter_map(|line| line.strip_prefix("prefix_rule(pattern="))
        .map(|line| {
            let pattern: Vec<String> =
                serde_json::from_str(line.split_once(", decision=").unwrap().0).unwrap();
            json!(format!(
                "Bash('{}' --plugin-state '{}' {} *)",
                pattern[0],
                pattern[2],
                pattern[3..].join(" ")
            ))
        })
        .collect();
    assert_eq!(&allow[3..], expected);
    assert!(!allow.iter().any(|entry| {
        entry
            .as_str()
            .is_some_and(|entry| entry.contains("cancel-held"))
    }));
}

#[test]
fn reactivation_is_idempotent_and_deactivation_keeps_identical_user_rules() {
    let f = Fixture::new();
    install_integration_package(
        &f.home,
        AgentRuntime::ClaudeCode,
        &fs::canonicalize(env!("CARGO_BIN_EXE_namedrop")).unwrap(),
        &f.root,
    )
    .unwrap();
    let user_rule = f.expected()[0].clone();
    // Start with no ownership record, as on a first activation with a user rule.
    fs::remove_file(f.root.join("claude-permissions.json")).unwrap();
    fs::write(
        f.settings(),
        json!({"permissions":{"allow":[user_rule,"Read"]},"model":"keep"}).to_string(),
    )
    .unwrap();
    f.activate();
    assert_eq!(
        f.read()["permissions"]["allow"].as_array().unwrap().len(),
        19
    );
    let before = fs::read(f.settings()).unwrap();
    let modified = fs::metadata(f.settings()).unwrap().modified().unwrap();
    f.activate();
    assert_eq!(fs::read(f.settings()).unwrap(), before);
    assert_eq!(
        fs::metadata(f.settings()).unwrap().modified().unwrap(),
        modified
    );
    f.deactivate();
    assert_eq!(
        f.read(),
        json!({"permissions":{"allow":[user_rule,"Read"]},"model":"keep"})
    );
    f.deactivate();
    assert_eq!(f.read()["permissions"]["allow"], json!([user_rule, "Read"]));
}

#[test]
fn runtime_path_change_removes_old_managed_rules_only() {
    let f = Fixture::new();
    fs::write(
        f.settings(),
        json!({"permissions":{"allow":["Read"]}}).to_string(),
    )
    .unwrap();
    f.activate();
    let first = f.root.join("old runtime/namedrop");
    install_integration_package(&f.home, AgentRuntime::ClaudeCode, &first, &f.root).unwrap();
    assert_eq!(
        f.read()["permissions"]["allow"].as_array().unwrap().len(),
        19
    );
    let next = f.root.join("next runtime/namedrop");
    install_integration_package(&f.home, AgentRuntime::ClaudeCode, &next, &f.root).unwrap();
    let allow = f.read()["permissions"]["allow"].as_array().unwrap().clone();
    assert_eq!(allow.len(), 19);
    assert_eq!(allow[0], "Read");
    assert!(
        !allow
            .iter()
            .any(|entry| entry.as_str().unwrap().contains("old runtime"))
    );
    assert_eq!(&allow[1..], f.expected_for(&next));
    f.deactivate();
    assert_eq!(f.read()["permissions"]["allow"], json!(["Read"]));
}

#[test]
fn unsafe_settings_are_skipped_untouched_without_failing_activation() {
    for invalid in [
        "{ invalid JSON",
        r#"{"permissions":{"allow":"Read"}}"#,
        r#"{"permissions":null}"#,
        "[]",
        r#"{"permissions":{"allow":[],"allow":["Read"]}}"#,
        r#"{"permissions":{},"permissions":{}}"#,
    ] {
        let f = Fixture::new();
        fs::write(f.settings(), invalid).unwrap();
        let result = f.activate();
        assert_eq!(result["activated"], true);
        assert_eq!(fs::read_to_string(f.settings()).unwrap(), invalid);
        let error = result["integration_packages"]["claude-code"]["permissions"]["error"]
            .as_str()
            .expect("activation must explain why settings were skipped");
        assert!(error.contains("settings.json"), "{error}");
        assert_permission_fallback(&f, &result);
    }
}

#[test]
fn doctor_reports_missing_claude_rules_with_activation_repair_without_writing() {
    let f = Fixture::new();
    f.activate();
    let mut settings = f.read();
    settings["permissions"]["allow"]
        .as_array_mut()
        .unwrap()
        .remove(0);
    fs::write(f.settings(), settings.to_string()).unwrap();
    let before = fs::read(f.settings()).unwrap();
    let report = f.doctor();
    let issue = report
        .issues
        .iter()
        .find(|issue| issue.explanation.contains("settings.json"))
        .expect("doctor must diagnose missing Claude rules");
    let serialized = serde_json::to_value(issue).unwrap();
    assert_eq!(serialized["repair"]["operation"], "activate");
    assert_eq!(serialized["repair"]["input"], json!({"confirmed":true}));
    assert_eq!(
        serialized["repair"]["command"],
        json!([
            fs::canonicalize(env!("CARGO_BIN_EXE_namedrop")).unwrap(),
            "--plugin-state",
            f.root,
            "activate",
            "--input-json",
            "{\"confirmed\":true}",
            "--herdr-socket",
            "socket-a",
            "--herdr-workspace",
            "workspace-1"
        ])
    );
    assert!(issue.explanation.contains("activate again"), "{issue:?}");
    assert_eq!(fs::read(f.settings()).unwrap(), before);
}

#[test]
fn config_directory_override_is_isolated_from_the_default_profile() {
    let mut f = Fixture::new();
    let default = f.home.join(".claude/settings.json");
    fs::write(&default, "default profile untouched").unwrap();
    f.claude = f.root.join("custom claude profile");
    fs::create_dir(&f.claude).unwrap();
    fs::write(
        f.settings(),
        json!({"permissions":{"allow":["Read"]}}).to_string(),
    )
    .unwrap();
    f.activate();
    assert_eq!(
        f.read()["permissions"]["allow"].as_array().unwrap().len(),
        19
    );
    f.deactivate();
    assert_eq!(f.read()["permissions"]["allow"], json!(["Read"]));
    assert_eq!(
        fs::read_to_string(default).unwrap(),
        "default profile untouched"
    );
}

#[test]
fn deactivation_preserves_a_user_added_duplicate_and_unrelated_entries() {
    let f = Fixture::new();
    f.activate();
    let mut settings = f.read();
    let duplicate = settings["permissions"]["allow"][0].clone();
    settings["permissions"]["allow"]
        .as_array_mut()
        .unwrap()
        .extend([duplicate.clone(), json!("Read")]);
    fs::write(f.settings(), settings.to_string()).unwrap();
    f.deactivate();
    assert_eq!(f.read()["permissions"]["allow"], json!([duplicate, "Read"]));
}

#[test]
fn settings_symlinks_and_invalid_ownership_are_refused_untouched() {
    let f = Fixture::new();
    let target = f.root.join("user-settings.json");
    fs::write(&target, "{}\n").unwrap();
    std::os::unix::fs::symlink(&target, f.settings()).unwrap();
    let result = f.activate();
    assert_eq!(result["activated"], true);
    assert_eq!(fs::read_to_string(&target).unwrap(), "{}\n");
    assert_eq!(
        result["integration_packages"]["claude-code"]["permissions"]["current"],
        false
    );
    assert_permission_fallback(&f, &result);
    fs::remove_file(f.settings()).unwrap();
    fs::write(f.settings(), "{}\n").unwrap();
    fs::write(
        f.root.join("claude-permissions.json"),
        "invalid ownership JSON",
    )
    .unwrap();
    f.activate();
    assert_eq!(fs::read_to_string(f.settings()).unwrap(), "{}\n");
}

#[test]
fn deactivation_does_not_recreate_settings_or_allow_removed_by_the_user() {
    for deleted_file in [false, true] {
        let f = Fixture::new();
        f.activate();
        if deleted_file {
            fs::remove_file(f.settings()).unwrap();
        } else {
            fs::write(f.settings(), "{\"model\":\"keep\"}\n").unwrap();
        }
        f.deactivate();
        if deleted_file {
            assert!(!f.settings().exists());
        } else {
            assert_eq!(
                fs::read_to_string(f.settings()).unwrap(),
                "{\"model\":\"keep\"}\n"
            );
        }
    }
}

#[test]
fn doctor_reports_stale_owned_claude_entries() {
    let f = Fixture::new();
    f.activate();
    let record = f.root.join("claude-permissions.json");
    let mut owned: Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    owned["entries"].as_array_mut().unwrap().push(json!(
        "Bash('/old/namedrop' --plugin-state '/old/state' reply *)"
    ));
    fs::write(&record, owned.to_string()).unwrap();
    let report = f.doctor();
    let issue = report
        .issues
        .iter()
        .find(|issue| issue.explanation.contains("stale managed Claude"))
        .unwrap();
    assert_eq!(
        serde_json::to_value(issue).unwrap()["repair"]["operation"],
        "activate"
    );
    assert_eq!(fs::read_to_string(record).unwrap(), owned.to_string());
}

#[test]
fn deactivating_one_of_two_worktrees_keeps_shared_permissions() {
    let f = Fixture::new();
    f.activate();
    f.run_for("workspace-2", "activate", json!({"confirmed":true}));
    let before = fs::read(f.settings()).unwrap();
    let first = f.run_for("workspace-1", "deactivate", json!({}));
    assert_eq!(first["claude_permissions"]["removed"], false);
    assert_eq!(fs::read(f.settings()).unwrap(), before);
    let second = f.run_for("workspace-2", "deactivate", json!({}));
    assert_eq!(second["claude_permissions"]["removed"], true);
    assert_eq!(second["claude_permissions"]["removed_entries"], 18);
    assert_eq!(f.read()["permissions"]["allow"], json!([]));
}

#[test]
fn a_pending_rename_is_recovered_without_orphaning_added_rules() {
    let f = Fixture::new();
    f.activate();
    let entries = f.expected();
    let after = fs::read_to_string(f.settings()).unwrap();
    fs::write(
        f.root.join("claude-permissions.json"),
        json!({
            "settings_path": f.settings(), "entries": [],
            "pending": {"next": {"settings_path": f.settings(), "entries": entries},
                "updates": [pending_update(&f.settings(),None,Some(&after),&entries)]}
        })
        .to_string(),
    )
    .unwrap();
    f.activate();
    f.deactivate();
    assert_eq!(f.read()["permissions"]["allow"], json!([]));
}

#[test]
fn deactivation_reports_no_removal_when_no_owned_rules_exist() {
    let f = Fixture::new();
    f.activate();
    f.deactivate();
    let report = f.run_for("workspace-1", "deactivate", json!({}));
    assert_eq!(report["claude_permissions"]["removed"], false);
}

#[test]
fn generated_rules_match_fixed_literals_and_exclude_every_management_write() {
    let f = Fixture::new();
    install_integration_package(
        &f.home,
        AgentRuntime::ClaudeCode,
        Path::new("/opt/namedrop"),
        &f.root,
    )
    .unwrap();
    let allow = f.read()["permissions"]["allow"].as_array().unwrap().clone();
    let state = f.root.to_string_lossy();
    assert_eq!(
        allow[0],
        format!("Bash('/opt/namedrop' --plugin-state '{state}' describe *)")
    );
    assert!(allow.contains(&json!(format!(
        "Bash('/opt/namedrop' --plugin-state '{state}' complete --no-reply *)"
    ))));
    assert!(allow.contains(&json!(format!(
        "Bash('/opt/namedrop' --plugin-state '{state}' address list *)"
    ))));
    for operation in [
        "group set",
        "group remove",
        "address persona",
        "handoff acknowledge",
        "delivery acknowledge",
        "delivery cancel-held",
    ] {
        assert!(!allow.contains(&json!(format!(
            "Bash('/opt/namedrop' --plugin-state '{state}' {operation} *)"
        ))));
    }
}

#[test]
fn input_file_errors_never_echo_unknown_keys_or_values() {
    let f = Fixture::new();
    for input in [
        r#"{"request_id":"r","source":"@agent SECRET_VALUE","author_kind":"SECRET_KIND","SECRET_FIELD":"SECRET_VALUE"}"#,
        r#"{"attempt_id":"a","SECRET_FIELD":"SECRET_VALUE"}"#,
        r#"{"SECRET_FIELD":"SECRET_VALUE"}"#,
        r#"{"runtime":"SECRET_VALUE","nonce":"SECRET_NONCE"}"#,
        r#"{"source":"SECRET_VALUE","author_kind":"SECRET_KIND"}"#,
        r#"{"SECRET_FIELD":"SECRET_VALUE", bad}"#,
    ] {
        let file = f.root.join("private-input.json");
        fs::write(&file, input).unwrap();
        for command in [
            vec!["send"],
            vec!["ack"],
            vec!["reply"],
            vec!["register"],
            vec!["handoff", "begin"],
        ] {
            let output = std::process::Command::new(env!("CARGO_BIN_EXE_namedrop"))
                .args(["--plugin-state", f.root.to_str().unwrap()])
                .args(command)
                .args([
                    "--input-json-file",
                    file.to_str().unwrap(),
                    "--herdr-socket",
                    "socket-a",
                    "--herdr-workspace",
                    "workspace-1",
                    "--herdr-pane",
                    "pane-1",
                ])
                .isolated_home(&f.home)
                .env("CLAUDE_CONFIG_DIR", f.home.join(".claude"))
                .env("CODEX_HOME", f.home.join(".codex"))
                .env("HERDR_BIN_PATH", f.root.join("herdr"))
                .env("TEST_WORKSPACE", "workspace-1")
                .output()
                .unwrap();
            assert!(!output.status.success());
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let report: Value = serde_json::from_slice(&output.stdout).unwrap();
            if report["error"]["message"]
                .as_str()
                .unwrap()
                .contains("invalid operation input: ")
            {
                panic!("schema details were not redacted: {output:?}");
            }
            for secret in [
                "SECRET_FIELD",
                "SECRET_VALUE",
                "SECRET_NONCE",
                "SECRET_KIND",
            ] {
                assert!(!text.contains(secret), "{text}");
            }
        }
    }
}

#[test]
fn an_inherited_claude_config_directory_is_not_used_by_in_process_helpers() {
    const CHILD: &str = "NAMEDROP_REVIEW_CONFIG_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let f = Fixture::new();
        install_integration_package(
            &f.home,
            AgentRuntime::ClaudeCode,
            Path::new("/opt/namedrop"),
            &f.root,
        )
        .unwrap();
        execute_lifecycle_operation("activate", json!({"confirmed":true}), &f.runtime()).unwrap();
        let sentinel =
            PathBuf::from(std::env::var_os("CLAUDE_CONFIG_DIR").unwrap()).join("settings.json");
        assert_eq!(fs::read_to_string(sentinel).unwrap(), "{\"parent\":true}\n");
        assert!(f.home.join(".claude/settings.json").exists());
        return;
    }
    let f = Fixture::new();
    let parent = f.root.join("parent-profile");
    fs::create_dir(&parent).unwrap();
    fs::write(parent.join("settings.json"), "{\"parent\":true}\n").unwrap();
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "an_inherited_claude_config_directory_is_not_used_by_in_process_helpers",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .isolated_home(&f.home)
        .env("CLAUDE_CONFIG_DIR", parent)
        .env("CODEX_HOME", f.home.join(".codex"))
        .output()
        .unwrap();
    assert!(result.status.success(), "{result:?}");
}

#[test]
fn a_pending_update_before_rename_keeps_a_preexisting_user_rule_unowned() {
    let f = Fixture::new();
    f.activate();
    let desired = f.expected();
    let user = desired[0].clone();
    let before = json!({"model":"keep","permissions":{"allow":[user]}}).to_string();
    fs::write(f.settings(), &before).unwrap();
    fs::remove_file(f.root.join("claude-permissions.json")).unwrap();
    f.activate();
    let after = fs::read_to_string(f.settings()).unwrap();
    fs::write(f.settings(), &before).unwrap();
    fs::write(
        f.root.join("claude-permissions.json"),
        json!({
            "settings_path":f.settings(),"entries":[],
            "pending":{"next":{"settings_path":f.settings(),"entries":&desired[1..]},
                "updates":[pending_update(&f.settings(),Some(&before),Some(&after),&desired[1..])]}
        })
        .to_string(),
    )
    .unwrap();
    f.activate();
    f.deactivate();
    assert_eq!(
        f.read(),
        json!({"model":"keep","permissions":{"allow":[user]}})
    );
}

#[test]
fn pending_recovery_discards_conflicting_updates_and_keeps_only_old_ownership() {
    for operation in ["activate", "deactivate"] {
        let f = Fixture::new();
        f.activate();
        let desired = f.expected();
        let old = desired[0].clone();
        let uncertain = desired[1].clone();
        let record=json!({"settings_path":f.settings(),"entries":[old,desired[2]],"pending":{
        "next":{"settings_path":f.settings(),"entries":desired},
        "updates":[pending_update(&f.settings(),Some("{}"),Some("{\"permissions\":{\"allow\":[]}}"),&desired)]
    }}).to_string();
        fs::write(f.root.join("claude-permissions.json"), record).unwrap();
        fs::write(
            f.settings(),
            json!({"permissions":{"allow":[old,uncertain,"Read"]},"user":true}).to_string(),
        )
        .unwrap();
        let output = f
            .command_for("workspace-1")
            .args([
                operation,
                "--input-json",
                if operation == "activate" {
                    r#"{"confirmed":true}"#
                } else {
                    "{}"
                },
            ])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("leftover rules may need review"),
            "{output:?}"
        );
        assert!(f.read()["user"].as_bool().unwrap());
        f.deactivate();
        assert_eq!(f.read()["permissions"]["allow"], json!([uncertain, "Read"]));
        let owned: Value =
            serde_json::from_slice(&fs::read(f.root.join("claude-permissions.json")).unwrap())
                .unwrap();
        assert!(owned.get("pending").is_none());
    }
}

#[test]
fn pending_recovery_discards_a_replay_with_a_different_serialization_hash() {
    for operation in ["activate", "deactivate"] {
        let f = Fixture::new();
        f.activate();
        let desired = f.expected();
        let old = desired[0].clone();
        let user = desired[1].clone();
        let added = desired[2].clone();
        let before = json!({"model":"keep","permissions":{"allow":[old,user,"Read"]}}).to_string();
        // An older serializer wrote compact JSON, while replay now emits pretty JSON.
        let after =
            json!({"model":"keep","permissions":{"allow":[old,user,"Read",added]}}).to_string();
        fs::write(f.settings(), &before).unwrap();
        fs::write(
            f.root.join("claude-permissions.json"),
            json!({
                "settings_path":f.settings(),"entries":[old,desired[3]],
                "pending":{"next":{"settings_path":f.settings(),"entries":[old,added]},
                    "updates":[pending_update(&f.settings(),Some(&before),Some(&after),&[added])]}
            })
            .to_string(),
        )
        .unwrap();
        let output = f
            .command_for("workspace-1")
            .args([
                operation,
                "--input-json",
                if operation == "activate" {
                    r#"{"confirmed":true}"#
                } else {
                    "{}"
                },
            ])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("leftover rules may need review"),
            "{output:?}"
        );
        let owned: Value =
            serde_json::from_slice(&fs::read(f.root.join("claude-permissions.json")).unwrap())
                .unwrap();
        assert!(owned.get("pending").is_none(), "{owned}");
        f.deactivate();
        assert_eq!(
            f.read(),
            json!({"model":"keep","permissions":{"allow":[user,"Read"]}})
        );
    }
}

#[test]
fn duplicate_settings_keys_are_refused_without_modification() {
    for text in [
        r#"{"permissions":{"allow":[]},"permissions":{"allow":["Read"]}}"#,
        r#"{"model":"first","model":"last"}"#,
        r#"{"permissions":{"allow":[],"allow":["Read"]}}"#,
    ] {
        let f = Fixture::new();
        fs::write(f.settings(), text).unwrap();
        let output = f
            .command_for("workspace-1")
            .args(["activate", "--input-json", r#"{"confirmed":true}"#])
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert_eq!(fs::read_to_string(f.settings()).unwrap(), text);
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("duplicate"),
            "{output:?}"
        );
    }
}

#[test]
fn input_file_operational_errors_match_inline_errors() {
    let f = Fixture::new();
    f.activate();
    let named = f
        .command_for("workspace-1")
        .args(["name", "@author", "--type", "agent"])
        .env("TEST_PEER_PANE", "1")
        .output()
        .unwrap();
    assert!(named.status.success(), "{named:?}");
    let peer = std::process::Command::new(env!("CARGO_BIN_EXE_namedrop"))
        .args([
            "--plugin-state",
            f.root.to_str().unwrap(),
            "--herdr-socket",
            "socket-a",
            "--herdr-workspace",
            "workspace-1",
            "--herdr-pane",
            "pane-2",
            "name",
            "@peer",
            "--type",
            "agent",
        ])
        .isolated_home(&f.home)
        .env("CLAUDE_CONFIG_DIR", &f.claude)
        .env("CODEX_HOME", f.home.join(".codex"))
        .env("HERDR_BIN_PATH", f.root.join("herdr"))
        .env("TEST_WORKSPACE", "workspace-1")
        .env("TEST_PEER_PANE", "1")
        .output()
        .unwrap();
    assert!(peer.status.success(), "{peer:?}");
    f.run_for(
        "workspace-1",
        "send",
        json!({"request_id":"reused","source":"@peer first"}),
    );
    for (operation, input, expected) in [
        ("activate", json!({"confirmed":false}), "confirmation"),
        (
            "send",
            json!({"request_id":"unknown","source":"@missing hello"}),
            "unknown Mention: @missing",
        ),
        (
            "send",
            json!({"request_id":"reused","source":"@peer changed"}),
            "different message content",
        ),
    ] {
        let inline = f
            .command_for("workspace-1")
            .args([operation, "--input-json", &input.to_string()])
            .output()
            .unwrap();
        let path = f.root.join("input.json");
        fs::write(&path, input.to_string()).unwrap();
        let file = f
            .command_for("workspace-1")
            .args([operation, "--input-json-file", path.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(!inline.status.success(), "{inline:?}");
        assert_eq!(file.status.code(), inline.status.code());
        assert_eq!(file.stdout, inline.stdout);
        assert_eq!(file.stderr, inline.stderr);
        assert!(
            String::from_utf8_lossy(&file.stdout).contains(expected),
            "{file:?}"
        );
    }
}

fn assert_permission_fallback(f: &Fixture, activation: &Value) {
    let permissions = &activation["integration_packages"]["claude-code"]["permissions"];
    let repair = permissions["repair"].as_str().unwrap();
    assert!(
        repair.contains("Claude agents will be prompted"),
        "{repair}"
    );
    assert!(
        repair.contains("settings") && repair.contains("reactivat"),
        "{repair}"
    );
    let skill =
        fs::read_to_string(package_directory(&f.home, AgentRuntime::ClaudeCode).join("SKILL.md"))
            .unwrap();
    assert!(!skill.contains("allowed-tools:"), "{skill}");
    assert!(
        skill.contains("reactivate") && skill.contains("grants no permissions"),
        "{skill}"
    );
    let report = f.doctor();
    let issue = report
        .issues
        .iter()
        .find(|issue| issue.explanation.contains("settings.json"))
        .unwrap();
    assert!(
        issue.explanation.contains("Claude agents will be prompted"),
        "{issue:?}"
    );
    assert!(
        issue.explanation.contains("settings") && issue.explanation.contains("reactivat"),
        "{issue:?}"
    );
}

#[test]
fn reactivation_upgrades_a_skill_with_allowed_tools_and_preserves_rule_ownership() {
    for registration_is_user_owned in [false, true] {
        let f = Fixture::new();
        f.activate();
        let legacy_rules = f.expected();
        let record_path = f.root.join("claude-permissions.json");
        let mut record: Value = serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
        let user_rule = f.expected()[1].clone();
        record["entries"] = json!(
            legacy_rules
                .iter()
                .filter(|rule| !registration_is_user_owned || *rule != &user_rule)
                .collect::<Vec<_>>()
        );
        fs::write(&record_path, record.to_string()).unwrap();
        let mut allow = legacy_rules.clone();
        allow.push(json!("Read"));
        fs::write(
            f.settings(),
            json!({"model":"keep", "permissions":{"allow":allow}}).to_string(),
        )
        .unwrap();
        let path = package_directory(&f.home, AgentRuntime::ClaudeCode).join("SKILL.md");
        let mut skill_rules = vec![f.bootstrap_rule()];
        skill_rules.extend(legacy_rules);
        fs::write(
            &path,
            format!(
                "---\nname: namedrop\nallowed-tools: {}\n---\nLegacy skill retains its rules.\n",
                json!(skill_rules)
            ),
        )
        .unwrap();

        f.activate();
        let updated = fs::read_to_string(&path).unwrap();
        assert!(!updated.contains("allowed-tools:"), "{updated}");
        assert!(updated.contains("grants no permissions"), "{updated}");
        let allow = f.read()["permissions"]["allow"].as_array().unwrap().clone();
        for rule in f.expected() {
            assert!(allow.contains(&rule), "missing {rule}");
        }
        let owned: Value = serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
        assert_eq!(
            owned["entries"].as_array().unwrap().contains(&user_rule),
            !registration_is_user_owned
        );
        let before = fs::read(f.settings()).unwrap();
        f.activate();
        assert_eq!(fs::read(f.settings()).unwrap(), before);
        f.deactivate();
        let expected = if registration_is_user_owned {
            json!([user_rule, "Read"])
        } else {
            json!(["Read"])
        };
        assert_eq!(
            f.read(),
            json!({"model":"keep", "permissions":{"allow":expected}})
        );
    }
}

#[test]
fn reactivation_removes_an_owned_environment_bootstrap_rule() {
    let f = Fixture::new();
    f.activate();
    let unsafe_rule = f.bootstrap_rule();
    let mut settings = f.read();
    let allow = settings["permissions"]["allow"].as_array_mut().unwrap();
    if !allow.contains(&unsafe_rule) {
        allow.push(unsafe_rule.clone());
    }
    allow.push(json!("Read"));
    fs::write(f.settings(), settings.to_string()).unwrap();
    let record_path = f.root.join("claude-permissions.json");
    let mut record: Value = serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    if !record["entries"].as_array().unwrap().contains(&unsafe_rule) {
        record["entries"]
            .as_array_mut()
            .unwrap()
            .push(unsafe_rule.clone());
    }
    fs::write(&record_path, record.to_string()).unwrap();
    f.activate();
    let settings = f.read();
    let allow = settings["permissions"]["allow"].as_array().unwrap();
    assert!(
        !allow.contains(&unsafe_rule),
        "reactivation retained the owned environment bootstrap: {allow:?}"
    );
    assert_eq!(&allow[..18], f.expected());
    assert_eq!(allow[18], "Read");
    let owned: Value = serde_json::from_slice(&fs::read(&record_path).unwrap()).unwrap();
    assert!(!owned["entries"].as_array().unwrap().contains(&unsafe_rule));
    f.deactivate();
    assert_eq!(f.read()["permissions"]["allow"], json!(["Read"]));
}

#[test]
fn registration_flags_override_every_inherited_transport_value() {
    let f = Fixture::new();
    let runtime = f.runtime();
    execute_lifecycle_operation("activate", json!({"confirmed":true}), &runtime).unwrap();
    execute_lifecycle_operation("address.create", json!({"role_name":"Worker", "role_description":"Tests registration flags", "alias":"worker"}), &runtime).unwrap();
    let namespace = StateNamespace::new("socket-a", SpaceId::new("workspace-1"));
    let address = StateStore::open(&f.root)
        .unwrap()
        .transaction(|tx| tx.list_pane_addresses(&namespace))
        .unwrap()
        .remove(0);
    let challenge =
        issue_registration_challenge(&address.id, AgentRuntime::ClaudeCode, &runtime).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_namedrop"))
        .args(["--plugin-state", f.root.to_str().unwrap(), "register", "--herdr-socket", "socket-a", "--herdr-workspace", "workspace-1", "--herdr-pane", "pane-1", "--input-json", &json!({"runtime":"claude-code", "protocol_version":"collab/v1", "nonce":challenge.nonce, "capabilities":complete_capability_profile()}).to_string()])
        .isolated_home(&f.home).env("CLAUDE_CONFIG_DIR", &f.claude).env("CODEX_HOME", f.home.join(".codex"))
        .env("HERDR_BIN_PATH", f.root.join("herdr")).env("TEST_WORKSPACE", "workspace-1")
        .env("HERDR_PLUGIN_STATE_DIR", f.root.join("wrong-state")).env("HERDR_SOCKET_PATH", "wrong-socket")
        .env("HERDR_WORKSPACE_ID", "wrong-workspace").env("HERDR_PANE_ID", "wrong-pane")
        .output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["data"]["tool"]["environment"],
        json!({"HERDR_PLUGIN_STATE_DIR":f.root,"HERDR_SOCKET_PATH":"socket-a","HERDR_WORKSPACE_ID":"workspace-1","HERDR_PANE_ID":"pane-1"})
    );
    assert!(!f.root.join("wrong-state").exists());
}
