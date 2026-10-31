#[path = "support/codex_app_server.rs"]
mod codex_app_server;
#[path = "support/update_profile.rs"]
mod update_profile;
use namedrop::*;
use serde_json::json;
use std::{
    cell::{Cell, RefCell},
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(1);

#[test]
fn cached_update_is_informational_and_doctor_remains_healthy_and_read_only() {
    let f = Fixture::new();
    if update_profile::isolate(&f.root, &f.home) {
        return;
    }
    f.register();
    let baseline = diagnose_collaboration(&f.runtime());
    assert_eq!(baseline.status, HealthStatus::Healthy, "{baseline:?}");
    let cache = json!({"schema_version":1,"checked_at":100,"last_success_at":90,"last_error":"offline",
        "source":{"owner":"owner","repo":"repo","requested_ref":"main","resolved_commit":"a".repeat(40),"subdir":""},
        "remote_commit":"b".repeat(40),"remote_version":"9.10.0"});
    fs::write(f.root.join("update-check.json"), cache.to_string()).unwrap();
    let before = fs::read(f.root.join("namedrop.sqlite3")).unwrap();
    let report = diagnose_collaboration(&f.runtime());
    assert_eq!(report.status, HealthStatus::Healthy);
    let update = report
        .issues
        .iter()
        .find(|issue| issue.code == "ND-UPDATE-001")
        .unwrap();
    assert_eq!(update.severity, HealthSeverity::Info);
    assert!(update.summary.contains("9.10.0"));
    assert!(update.summary.contains(env!("CARGO_PKG_VERSION")));
    assert_eq!(update.diagnostic["last_success_at"], 90);
    assert_eq!(update.diagnostic["last_error"], "offline");
    let text = render_health_report_to_text(&report);
    assert!(text.starts_with(&render_health_report_to_text(&baseline)));
    assert!(text.contains("herdr plugin action invoke update --plugin namedrop"));
    assert_eq!(before, fs::read(f.root.join("namedrop.sqlite3")).unwrap());
    assert!(!f.root.join("update-notice.json").exists());
    let runtime = LifecycleRuntime {
        workspace_id: Some("workspace-1"),
        ..f.runtime()
    };
    assert!(
        request_doctor_assistance(&runtime, "session-old")
            .unwrap_err()
            .contains("No issues remain")
    );

    for version in [
        env!("CARGO_PKG_VERSION"),
        "0.99.0",
        "9.0.0\nmalice",
        "9.0.0\"",
        "\u{1b}[31m9.0.0",
        &"9".repeat(10_000),
    ] {
        let mut cache = cache.clone();
        cache["remote_version"] = version.into();
        fs::write(f.root.join("update-check.json"), cache.to_string()).unwrap();
        assert!(
            !diagnose_collaboration(&f.runtime())
                .issues
                .iter()
                .any(|issue| issue.code == "ND-UPDATE-001")
        );
    }
}

#[test]
fn invalid_update_settings_are_visible_in_doctor_without_a_health_failure() {
    let f = Fixture::new();
    if update_profile::isolate(&f.root, &f.home) {
        return;
    }
    f.register();
    let path = f
        .home
        .join(".config/herdr/plugins/config/namedrop/settings.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    for settings in [
        "{",
        "{\"check_for_update\":false}",
        "{\"check_for_updates\":\"false\"}",
    ] {
        fs::write(&path, settings).unwrap();
        let report = diagnose_collaboration(&f.runtime());
        assert_eq!(report.status, HealthStatus::Healthy);
        let setting = report
            .issues
            .iter()
            .find(|issue| issue.code == "ND-UPDATE-002")
            .unwrap();
        assert_eq!(setting.severity, HealthSeverity::Info);
        assert_eq!(setting.diagnostic["path"], path.to_string_lossy().as_ref());
    }
    fs::write(path, "{\"check_for_updates\":false}").unwrap();
    assert!(
        !diagnose_collaboration(&f.runtime())
            .issues
            .iter()
            .any(|issue| issue.code.starts_with("ND-UPDATE-"))
    );
}

struct Fixture {
    root: PathBuf,
    home: PathBuf,
    host: Host,
    ids: Ids,
}
impl Fixture {
    fn new() -> Self {
        let root = update_profile::root(std::env::temp_dir().join(format!(
            "namedrop-health-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )));
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        Self {
            root,
            home,
            host: Host {
                session: RefCell::new("session-old".into()),
                missing_session: Cell::new(false),
                prompts: RefCell::new(Vec::new()),
                prompt_error: RefCell::new(None),
                plugin_root: RefCell::new(None),
                plugin_lists: Cell::new(0),
            },
            ids: Ids(AtomicU64::new(1)),
        }
    }
    fn runtime(&self) -> LifecycleRuntime<'_, Host, Time, Ids> {
        LifecycleRuntime {
            state_directory: &self.root,
            home_directory: Some(&self.home),
            agent_config: namedrop::AgentConfigDirectories::default(),
            cwd: Path::new("/repo"),
            workspace_id: None,
            current_pane_id: Some("pane-1"),
            session_socket_id: "socket-a",
            herdr: &self.host,
            clock: &Time,
            ids: &self.ids,
        }
    }
    fn register(&self) {
        let ns = StateNamespace::for_space("socket-a", "workspace-1");
        let attr = MutationAttribution::new(ActorKind::Human, "test", 100);
        let address = PaneAddressId::new("reviewer");
        StateStore::open(&self.root)
            .unwrap()
            .transaction(|tx| {
                tx.activate_worktree(&ns, &attr)?;
                tx.create_pane_address(
                    &ns,
                    &PaneAddressDraft {
                        id: address.clone(),
                        role_id: PaneRoleId::new("role"),
                        role_name: "Reviewer".into(),
                        role_description: "Review code".into(),
                        alias: "reviewer".into(),
                    },
                    &attr,
                )?;
                tx.bind_pane(
                    &ns,
                    &PaneBindingId::new("binding-1"),
                    &address,
                    "pane-1",
                    &attr,
                )?;
                tx.observe_agent_session(
                    &ns,
                    "observation-1",
                    &address,
                    &AgentSessionId::new("session-old"),
                    "idle",
                    &attr,
                )?;
                tx.complete_reconciliation(&ns)?;
                Ok(())
            })
            .unwrap();
        let challenge =
            issue_registration_challenge(&address, AgentRuntime::ClaudeCode, &self.runtime())
                .unwrap();
        execute_integration_operation("register",json!({"runtime":"claude-code","protocol_version":"collab/v1","nonce":challenge.nonce,"capabilities":complete_capability_profile()}),&self.runtime()).unwrap();
        install_integration_package(
            &self.home,
            AgentRuntime::ClaudeCode,
            &fs::canonicalize(std::env::current_exe().unwrap()).unwrap(),
            &self.root,
        )
        .unwrap();
    }
}
impl Fixture {
    fn stage_runtime(&self, active: &[u8], built: Option<&[u8]>, launcher: bool) -> PathBuf {
        use sha2::Digest;
        let digest = namedrop::lower_hex(&sha2::Sha256::digest(active));
        let runtime = self.root.join("runtime");
        let release = runtime.join("releases").join(&digest);
        fs::create_dir_all(&release).unwrap();
        fs::write(release.join("namedrop"), active).unwrap();
        std::os::unix::fs::symlink(format!("releases/{digest}"), runtime.join("current")).unwrap();
        if launcher {
            fs::write(runtime.join("namedrop"), "#!/bin/sh\n").unwrap();
        }
        let root = self.root.join("plugin");
        fs::create_dir_all(root.join("bin")).unwrap();
        if let Some(built) = built {
            fs::write(root.join("bin/namedrop"), built).unwrap();
        }
        *self.host.plugin_root.borrow_mut() = Some(root.clone());
        root
    }
}
fn stale_runtime(report: &HealthReport) -> Option<&HealthIssue> {
    report
        .issues
        .iter()
        .find(|issue| issue.code == "ND-SETUP-004")
}
impl Drop for Fixture {
    fn drop(&mut self) {
        update_profile::cleanup(&self.root);
    }
}
struct Host {
    session: RefCell<String>,
    missing_session: Cell<bool>,
    prompts: RefCell<Vec<(String, String)>>,
    prompt_error: RefCell<Option<String>>,
    plugin_root: RefCell<Option<PathBuf>>,
    plugin_lists: Cell<u32>,
}
impl HerdrCommandExecutor for Host {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let mut stdout = match args {
            ["workspace", "list"] => {
                json!({"result":{"workspaces":[{"workspace_id":"workspace-1"}]}})
            }
            ["worktree", "list", "--cwd", "/repo", "--json"]
            | ["worktree", "list", "--json"]
            | ["worktree", "list", "--workspace", "workspace-1", "--json"] => {
                json!({"result":{"worktrees":[{"path":"/repo","open_workspace_id":"workspace-1"}]}})
            }
            ["pane", "list", "--workspace", "workspace-1"] => {
                json!({"result":{"panes":[{"pane_id":"pane-1","terminal_id":"terminal-1","tab_id":"tab-1","workspace_id":"workspace-1","agent":"claude","agent_session":{"value":self.session.borrow().clone()},"agent_status":"idle"}]}})
            }
            ["pane", "report-metadata", ..] => json!({}),
            ["agent", "prompt", pane_id, prompt] => {
                self.prompts
                    .borrow_mut()
                    .push(((*pane_id).to_owned(), (*prompt).to_owned()));
                if let Some(error) = self.prompt_error.borrow().clone() {
                    return Err(error);
                }
                json!({})
            }
            ["plugin", "list", "--plugin", "namedrop", "--json"] => {
                self.plugin_lists.set(self.plugin_lists.get() + 1);
                match self.plugin_root.borrow().as_ref() {
                    Some(root) => {
                        json!({"result":{"type":"plugin_list","plugins":[{"plugin_id":"namedrop","plugin_root":root}]}})
                    }
                    None => return Err("plugin namedrop is not installed".into()),
                }
            }
            _ => return Err(format!("unexpected command {args:?}")),
        };
        if self.missing_session.get() && args.starts_with(&["pane", "list"]) {
            stdout["result"]["panes"][0]["agent_session"] = serde_json::Value::Null;
        }
        Ok(HerdrCommandOutput {
            stdout: stdout.to_string(),
        })
    }
}
struct Time;
impl Clock for Time {
    fn now_unix_ms(&self) -> u64 {
        100
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
fn doctor_rejects_saved_registration_after_live_session_changes_without_mutation() {
    let f = Fixture::new();
    f.register();
    let before = diagnose_collaboration(&f.runtime());
    assert_eq!(before.available_agents, 1);
    *f.host.session.borrow_mut() = "session-new".into();
    let database = fs::read(f.root.join("namedrop.sqlite3")).unwrap();
    let after = diagnose_collaboration(&f.runtime());
    assert_eq!(
        after.available_agents, 0,
        "a saved registration cannot establish the live session's readiness"
    );
    let issue = after
        .issues
        .iter()
        .find(|issue| issue.code == "ND-SESSION-001")
        .unwrap();
    assert_eq!(
        serde_json::to_value(issue).unwrap()["diagnostic"]["pane_id"],
        "pane-1"
    );
    assert_eq!(fs::read(f.root.join("namedrop.sqlite3")).unwrap(), database);
}

#[test]
fn doctor_targets_unnamed_panes_and_known_setup_actions() {
    let f = Fixture::new();
    let inactive = serde_json::to_value(diagnose_collaboration(&f.runtime())).unwrap();
    assert_eq!(inactive["issues"][0]["repair"]["operation"], "activate");
    execute_lifecycle_operation("activate", json!({"confirmed":true}), &f.runtime()).unwrap();
    let report = diagnose_collaboration(&f.runtime());
    let unnamed = report
        .issues
        .iter()
        .find(|issue| issue.code == "ND-SETUP-002")
        .unwrap();
    let value = serde_json::to_value(unnamed).unwrap();
    assert_eq!(value["diagnostic"]["pane_id"], "pane-1");
    assert_eq!(value["diagnostic"]["workspace_id"], "workspace-1");
    assert_eq!(value["diagnostic"]["runtime"], "claude-code");
    assert!(unnamed.repair.prompt.contains("pane-1"));
    execute_control_operation("worktree.pause", json!({}), &f.runtime()).unwrap();
    let report = diagnose_collaboration(&f.runtime());
    let paused = serde_json::to_value(
        report
            .issues
            .iter()
            .find(|issue| issue.code == "ND-SPACE-001")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(paused["repair"]["operation"], "worktree.resume");
    let argv = paused["repair"]["command"].as_array().unwrap();
    assert!(
        argv.windows(2)
            .any(|words| words == [json!("space"), json!("resume")])
    );
}

#[test]
fn doctor_explains_missing_herdr_native_session_identity() {
    let f = Fixture::new();
    f.register();
    f.host.missing_session.set(true);
    let report = diagnose_collaboration(&f.runtime());
    let issue = report
        .issues
        .iter()
        .find(|issue| issue.code == "ND-SESSION-002")
        .unwrap();
    assert_eq!(
        serde_json::to_value(issue).unwrap()["repair"]["command"],
        json!(["herdr", "integration", "install", "claude"])
    );
    assert_eq!(report.available_agents, 0);
}

#[test]
fn doctor_checks_current_package_files_and_preserves_the_exact_error_read_only() {
    let f = Fixture::new();
    f.register();
    let hook =
        package_directory(&f.home, AgentRuntime::ClaudeCode).join("scripts/user-prompt-submit");
    fs::remove_file(&hook).unwrap();
    let database = fs::read(f.root.join("namedrop.sqlite3")).unwrap();
    let report = diagnose_collaboration(&f.runtime());
    assert_eq!(report.status, HealthStatus::NeedsAttention);
    assert_eq!(
        report.available_agents, 0,
        "registration alone cannot establish package readiness"
    );
    let issue = report
        .issues
        .iter()
        .find(|issue| issue.code == "ND-INTEGRATION-001")
        .unwrap();
    assert!(
        issue.explanation.contains(hook.to_str().unwrap()),
        "{issue:?}"
    );
    assert_eq!(
        serde_json::to_value(issue).unwrap()["diagnostic"]["source_id"],
        "integration-package-claude-code"
    );
    assert!(!hook.exists(), "doctor must not reinstall missing files");
    assert_eq!(fs::read(f.root.join("namedrop.sqlite3")).unwrap(), database);
}

#[test]
fn doctor_uses_current_package_evidence_without_rewriting_stale_attention() {
    let f = Fixture::new();
    f.register();
    let ns = StateNamespace::for_space("socket-a", "workspace-1");
    StateStore::open(&f.root)
        .unwrap()
        .transaction(|tx| {
            tx.open_attention_item(
                &ns,
                &AttentionItemDraft {
                    id: AttentionItemId::new("old-install-error"),
                    source_kind: AttentionSourceKind::IntegrationRegistration,
                    source_id: "integration-package-claude-code".into(),
                    summary: "Old failed installation".into(),
                },
                &MutationAttribution::new(ActorKind::Plugin, "test", 100),
            )
        })
        .unwrap();
    let database = fs::read(f.root.join("namedrop.sqlite3")).unwrap();
    let report = diagnose_collaboration(&f.runtime());
    assert_eq!(
        report.status,
        HealthStatus::Healthy,
        "current files should supersede stale saved installation errors"
    );
    assert_eq!(fs::read(f.root.join("namedrop.sqlite3")).unwrap(), database);
}

#[test]
fn doctor_names_failed_context_sharing_and_how_to_retry() {
    let f = Fixture::new();
    f.register();
    StateStore::open(&f.root)
        .unwrap()
        .transaction(|tx| {
            tx.open_attention_item(
                &StateNamespace::for_space("socket-a", "workspace-1"),
                &AttentionItemDraft {
                    id: AttentionItemId::new("failed-context"),
                    source_kind: AttentionSourceKind::ContextGrant,
                    source_id: "grant-1".into(),
                    summary: "Context Grant activation failed: snapshot exceeds 65536 bytes".into(),
                },
                &MutationAttribution::new(ActorKind::Plugin, "test", 100),
            )
        })
        .unwrap();
    let report = diagnose_collaboration(&f.runtime());
    let issue = report
        .issues
        .iter()
        .find(|issue| issue.code == "ND-CONTEXT-001")
        .unwrap();
    assert_eq!(issue.summary, "Shared context failed.");
    assert!(issue.explanation.contains("65536 bytes"));
    assert_eq!(
        issue.repair.prompt,
        "Ask the sender to fix the problem and share the context again?"
    );
}

#[test]
fn doctor_keeps_individual_attention_causes_and_operation_inputs() {
    let f = Fixture::new();
    f.register();
    let ns = StateNamespace::for_space("socket-a", "workspace-1");
    StateStore::open(&f.root)
        .unwrap()
        .transaction(|tx| {
            for id in ["thread-a", "thread-b"] {
                tx.open_attention_item(
                    &ns,
                    &AttentionItemDraft {
                        id: AttentionItemId::new(format!("attention-{id}")),
                        source_kind: AttentionSourceKind::AutomaticReplyLimit,
                        source_id: id.into(),
                        summary: format!("{id} reached its reply limit"),
                    },
                    &MutationAttribution::new(ActorKind::Plugin, "test", 100),
                )?;
            }
            Ok(())
        })
        .unwrap();
    let report = diagnose_collaboration(&f.runtime());
    let issues = report
        .issues
        .iter()
        .filter(|issue| issue.code == "ND-THREAD-001")
        .collect::<Vec<_>>();
    assert_eq!(
        issues.len(),
        2,
        "different threads need different decisions"
    );
    for id in ["thread-a", "thread-b"] {
        let issue = issues
            .iter()
            .find(|issue| issue.explanation.contains(id))
            .unwrap();
        let value = serde_json::to_value(issue).unwrap();
        assert_eq!(value["diagnostic"]["source_id"], id);
        assert_eq!(value["repair"]["operation"], "thread.continue");
        assert_eq!(value["repair"]["input"]["thread_id"], id);
        assert!(
            value["repair"]["command"]
                .as_array()
                .unwrap()
                .iter()
                .any(|arg| arg == "--plugin-state")
        );
    }
}

#[test]
fn doctor_keeps_secret_shaped_targets_and_repair_commands_exact() {
    let f = Fixture::new();
    f.register();
    let secret = "sk-abcdefghijklmnopqrstuvwxyz0123456789";
    StateStore::open(&f.root)
        .unwrap()
        .transaction(|tx| {
            tx.open_attention_item(
                &StateNamespace::for_space("socket-a", "workspace-1"),
                &AttentionItemDraft {
                    id: AttentionItemId::new("redact-id"),
                    source_kind: AttentionSourceKind::AutomaticReplyLimit,
                    source_id: secret.into(),
                    summary: format!("Failed request {secret}"),
                },
                &MutationAttribution::new(ActorKind::Plugin, "test", 100),
            )
        })
        .unwrap();
    let report = diagnose_collaboration(&f.runtime());
    let issue = report
        .issues
        .iter()
        .find(|issue| issue.code == "ND-THREAD-001")
        .unwrap();
    assert_eq!(issue.diagnostic["source_id"], secret);
    assert_eq!(issue.repair.operation.as_deref(), Some("thread.continue"));
    assert_eq!(issue.repair.input, Some(json!({"thread_id":secret})));
    let command = issue.repair.command.as_ref().unwrap();
    assert!(command.windows(2).any(|args| {
        args[0] == "--input-json"
            && serde_json::from_str::<serde_json::Value>(&args[1]).unwrap()
                == json!({"thread_id":secret})
    }));
    assert!(!issue.explanation.contains(secret));
    let rendered = render_health_report_to_text(&report);
    assert!(rendered.contains(&format!("source_id={secret}")));
    assert!(rendered.contains("Command:"));
}

#[test]
fn human_doctor_shows_exact_targets_quoted_commands_and_hook_setup_steps() {
    let f = Fixture::new();
    f.register();
    let target = "thread with 'quotes' and $(printf unsafe)";
    StateStore::open(&f.root)
        .unwrap()
        .transaction(|tx| {
            tx.open_attention_item(
                &StateNamespace::for_space("socket-a", "workspace-1"),
                &AttentionItemDraft {
                    id: AttentionItemId::new("human-visible-attention"),
                    source_kind: AttentionSourceKind::AutomaticReplyLimit,
                    source_id: target.into(),
                    summary: "Automatic replies reached their human checkpoint".into(),
                },
                &MutationAttribution::new(ActorKind::Plugin, "test", 100),
            )
        })
        .unwrap();
    let mut report = diagnose_collaboration(&f.runtime());
    let issue = report
        .issues
        .iter()
        .find(|issue| issue.code == "ND-THREAD-001")
        .unwrap();
    let expected_argv = issue.repair.command.clone().unwrap();
    let rendered = render_health_report_to_text(&report);
    assert!(rendered.contains("human-visible-attention"), "{rendered}");
    assert!(rendered.contains(target), "{rendered}");
    let command = rendered
        .lines()
        .find_map(|line| line.strip_prefix("Command: "))
        .expect("interactive doctor must expose its safe recovery command");
    // Parse the offered command through the actual shell without invoking it.
    let output = std::process::Command::new("/bin/sh")
        .args(["-c", &format!("set -- {command}; printf '%s\\n' \"$@\"")])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        expected_argv
    );
    let mut hook = report.issues[0].clone();
    hook.code = "ND-HOOK-001".into();
    hook.repair.command = None;
    hook.diagnostic = json!({"steps":["codex plugin add namedrop@namedrop --json","start a new Codex session and approve Namedrop in /hooks"]});
    report.issues = vec![hook];
    let rendered = render_health_report_to_text(&report);
    assert!(rendered.contains("codex plugin add namedrop@namedrop --json"));
    assert!(rendered.contains("approve Namedrop in /hooks"));
}

struct CodexHost<'a>(&'a Host);
impl HerdrCommandExecutor for CodexHost<'_> {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let mut output = self.0.execute(args)?;
        if args.starts_with(&["pane", "list"]) {
            let mut value: serde_json::Value = serde_json::from_str(&output.stdout).unwrap();
            value["result"]["panes"][0]["agent"] = json!("codex");
            output.stdout = value.to_string();
        }
        Ok(output)
    }
}

#[cfg(target_os = "linux")]
#[test]
fn doctor_reports_unsupported_codex_without_preparing_its_home() {
    // A subprocess keeps the executable override away from parallel tests.
    if std::env::var_os("NAMEDROP_DOCTOR_UNSUPPORTED_CHILD").is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "doctor_reports_unsupported_codex_without_preparing_its_home",
                "--nocapture",
            ])
            .env("NAMEDROP_DOCTOR_UNSUPPORTED_CHILD", "1")
            .env("NAMEDROP_AGENT_STATUS_CODEX_BIN", "/bin/sh")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    namedrop::linux_sandbox::check_support().expect("Linux tests require Landlock");
    let f = Fixture::new();
    f.register();
    let host = CodexHost(&f.host);
    let runtime = LifecycleRuntime {
        state_directory: &f.root,
        home_directory: Some(&f.home),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-1"),
        session_socket_id: "socket-a",
        herdr: &host,
        clock: &Time,
        ids: &f.ids,
    };
    let report = diagnose_collaboration(&runtime);
    let issue = report
        .issues
        .iter()
        .find(|issue| issue.code == "ND-MODEL-001")
        .expect("unsupported Codex diagnostic");
    assert!(format!("{issue:?}").contains("native static Linux Codex binary"));
    assert!(
        !f.home.join(".codex").exists(),
        "Doctor must not prepare a runtime home"
    );
}

#[test]
fn doctor_keeps_secret_shaped_paths_and_setup_commands_exact() {
    let f = Fixture::new();
    let secret = "sk-abcdefghijklmnopqrstuvwxyz0123456789";
    let home = f.root.join(secret);
    fs::create_dir(&home).unwrap();
    StateStore::open(&f.root)
        .unwrap()
        .transaction(|tx| {
            tx.activate_worktree(
                &StateNamespace::for_space("socket-a", "workspace-1"),
                &MutationAttribution::new(ActorKind::Human, "test", 100),
            )
        })
        .unwrap();
    install_integration_package(
        &home,
        AgentRuntime::Codex,
        &fs::canonicalize(std::env::current_exe().unwrap()).unwrap(),
        &f.root,
    )
    .unwrap();
    let host = CodexHost(&f.host);
    let runtime = LifecycleRuntime {
        state_directory: &f.root,
        home_directory: Some(&home),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-1"),
        session_socket_id: "socket-a",
        herdr: &host,
        clock: &Time,
        ids: &f.ids,
    };
    let report = diagnose_collaboration(&runtime);
    let hook = report
        .issues
        .iter()
        .find(|issue| issue.code == "ND-HOOK-001")
        .unwrap();
    let expected_step = format!(
        "codex plugin marketplace add '{}' --json",
        home.join(".agents/skills/namedrop").display()
    );
    assert!(
        hook.diagnostic["steps"]
            .as_array()
            .unwrap()
            .contains(&json!(expected_step)),
        "keep the exact setup path even when it resembles a credential: {hook:?}"
    );
    assert!(
        hook.diagnostic["steps"]
            .to_string()
            .contains("codex plugin add namedrop@namedrop --json")
    );
    let rendered = render_health_report_to_text(&report);
    assert!(rendered.contains(&expected_step));
    assert!(rendered.contains("codex plugin add namedrop@namedrop --json"));
    assert!(rendered.contains("approve Namedrop in /hooks"));
}

#[test]
fn doctor_unavailable_preserves_the_underlying_cause() {
    struct Broken;
    impl HerdrCommandExecutor for Broken {
        fn execute(&self, _: &[&str]) -> Result<HerdrCommandOutput, String> {
            Err("socket /tmp/missing-herdr.sock refused the connection\nAPI_TOKEN=secret-should-not-leak".into())
        }
    }
    let f = Fixture::new();
    let runtime = LifecycleRuntime {
        herdr: &Broken,
        state_directory: &f.root,
        home_directory: Some(&f.home),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-1"),
        session_socket_id: "socket-a",
        clock: &Time,
        ids: &f.ids,
    };
    let report = diagnose_collaboration(&runtime);
    assert_eq!(report.status, HealthStatus::Unavailable);
    assert!(
        report.issues[0]
            .explanation
            .contains("/tmp/missing-herdr.sock")
    );
    assert!(
        !serde_json::to_string(&report)
            .unwrap()
            .contains("secret-should-not-leak")
    );
    assert!(!f.root.join("namedrop.sqlite3").exists());
}

#[test]
fn name_and_activate_preserve_live_dispatch_leases_but_explicit_reconcile_recovers_them() {
    for operation in ["name", "activate"] {
        let f = Fixture::new();
        f.register();
        let ns = StateNamespace::for_space("socket-a", "workspace-1");
        let attr = MutationAttribution::new(ActorKind::Human, "test", 100);
        StateStore::open(&f.root)
            .unwrap()
            .transaction(|tx| {
                tx.create_pane_address(
                    &ns,
                    &PaneAddressDraft {
                        id: PaneAddressId::new("sender"),
                        role_id: PaneRoleId::new("sender-role"),
                        role_name: "Sender".into(),
                        role_description: "Test sender".into(),
                        alias: "sender".into(),
                    },
                    &attr,
                )?;
                tx.create_thread(&ns, &ThreadId::new("thread-lease"), "Live lease", &attr)?;
                tx.append_message(
                    &ns,
                    &MessageDraft {
                        thread_id: ThreadId::new("thread-lease"),
                        message_id: MessageId::new("message-lease"),
                        in_reply_to: None,
                        author_address_id: PaneAddressId::new("sender"),
                        author_kind: ActorKind::Human,
                        author_alias_snapshot: "sender".into(),
                        author_agent_session_snapshot: None,
                        request_id: "request-lease".into(),
                        authored_source: "@reviewer inspect this".into(),
                        body: "inspect this".into(),
                        grammar_version: "mention/v1".into(),
                        recipients: vec![MessageRecipientDraft {
                            address_id: PaneAddressId::new("reviewer"),
                            alias_snapshot: "reviewer".into(),
                            agent_session_snapshot: Some(AgentSessionId::new("session-old")),
                            delivery_id: DeliveryId::new("delivery-lease"),
                        }],
                        context_references: vec![],
                    },
                    &attr,
                )?;
                tx.lease_mailbox_head(
                    &ns,
                    &PaneAddressId::new("reviewer"),
                    &LeaseDraft {
                        attempt_id: DeliveryAttemptId::new("attempt-live"),
                        binding_id: PaneBindingId::new("binding-1"),
                        agent_session_id: AgentSessionId::new("session-old"),
                        retry_of_attempt_id: None,
                        owner: "live-worker".into(),
                        acquired_at_ms: 100,
                        expires_at_ms: 100000,
                    },
                    &attr,
                )?;
                Ok(())
            })
            .unwrap();
        let input = if operation == "name" {
            json!({"alias":"reviewer"})
        } else {
            json!({"confirmed":true})
        };
        execute_lifecycle_operation(operation, input, &f.runtime()).unwrap();
        let connection = rusqlite::Connection::open(f.root.join("namedrop.sqlite3")).unwrap();
        let state: String = connection
            .query_row(
                "SELECT state FROM delivery_attempt_state WHERE attempt_id='attempt-live'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            state, "dispatching",
            "{operation} must not apply restart recovery to a live worker's lease"
        );
        execute_lifecycle_operation("reconcile", json!({}), &f.runtime()).unwrap();
        let state: String = connection
            .query_row(
                "SELECT state FROM delivery_attempt_state WHERE attempt_id='attempt-live'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            state, "held",
            "explicit startup reconciliation retains conservative crash recovery"
        );
    }
}

#[test]
fn doctor_assistance_prompts_only_the_bound_live_session_and_never_claims_delivery() {
    let f = Fixture::new();
    f.register();
    let runtime = LifecycleRuntime {
        workspace_id: Some("workspace-1"),
        ..f.runtime()
    };
    assert!(
        request_doctor_assistance(&f.runtime(), "session-old").is_err(),
        "the popup's invoking space is required"
    );
    let unidentified = request_doctor_assistance(&runtime, "").unwrap_err();
    assert!(
        unidentified.starts_with("Submit the first prompt in this pane"),
        "the first turn, not a reinstall, is the first remedy: {unidentified}"
    );
    assert!(
        unidentified.contains("still missing after a turn"),
        "reinstalling stays the later remedy: {unidentified}"
    );
    assert!(
        f.host.prompts.borrow().is_empty(),
        "a pane without a captured identity must not be prompted"
    );

    let quiet = request_doctor_assistance(&runtime, "session-old").unwrap_err();
    assert!(quiet.contains("No issues remain"), "{quiet}");
    assert!(
        f.host.prompts.borrow().is_empty(),
        "a healthy space must not prompt the agent"
    );

    execute_control_operation("worktree.pause", json!({}), &runtime).unwrap();
    request_doctor_assistance(&runtime, "session-old").unwrap();
    let (pane_id, prompt) = f.host.prompts.borrow().last().cloned().unwrap();
    assert_eq!(f.host.prompts.borrow().len(), 1);
    assert_eq!(pane_id, "pane-1", "only the invoking pane may be prompted");
    assert!(prompt.contains("session-old"), "{prompt}");
    assert!(prompt.contains("ND-SPACE-001"), "{prompt}");
    assert!(prompt.contains("Never cancel a held delivery"), "{prompt}");
    assert!(
        prompt.contains("BEGIN UNTRUSTED DIAGNOSTIC DATA"),
        "issue text reaches the agent marked as data, not instructions: {prompt}"
    );

    *f.host.prompt_error.borrow_mut() = Some("herdr refused".into());
    let failure = request_doctor_assistance(&runtime, "session-old").unwrap_err();
    assert!(failure.starts_with("Unconfirmed"), "{failure}");
    assert_eq!(f.host.prompts.borrow().len(), 2);
}

#[test]
fn doctor_assistance_explains_unsafe_lock_permissions_without_internal_terms() {
    use std::os::unix::fs::PermissionsExt;

    let f = Fixture::new();
    f.register();
    let runtime = LifecycleRuntime {
        workspace_id: Some("workspace-1"),
        ..f.runtime()
    };
    execute_control_operation("worktree.pause", json!({}), &runtime).unwrap();
    let directory = f.root.join("pane-effects");
    fs::create_dir_all(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o777)).unwrap();

    let error = request_doctor_assistance(&runtime, "session-old").unwrap_err();
    assert_eq!(
        error,
        "Namedrop's pane lock for pane-1 is not private to you"
    );
    assert!(!error.contains("pane effects"), "{error}");
    assert!(f.host.prompts.borrow().is_empty());
}

/// A Doctor popup can sit open while its pane starts a different Agent Session
/// in the same terminal. Every read taken when the human presses the key sees
/// only the replacement, so the session captured at launch is what makes them
/// distinguishable.
#[test]
fn doctor_assistance_refuses_a_session_that_replaced_the_one_doctor_opened_for() {
    let f = Fixture::new();
    f.register();
    let runtime = LifecycleRuntime {
        workspace_id: Some("workspace-1"),
        ..f.runtime()
    };
    let opened_for = "session-old".to_owned();
    execute_control_operation("worktree.pause", json!({}), &runtime).unwrap();

    // The popup stays open; the pane restarts under a new Agent Session.
    *f.host.session.borrow_mut() = "session-new".into();

    let refused = request_doctor_assistance(&runtime, &opened_for).unwrap_err();
    assert!(refused.contains("changed session"), "{refused}");
    assert!(
        f.host.prompts.borrow().is_empty(),
        "the replacement session must not be prompted"
    );
    request_doctor_assistance(&runtime, "session-new")
        .expect("a popup opened for the live session still sends");
}

#[test]
fn doctor_after_reconciliation_clears_only_completed_settled_report_attention() {
    for outcome in ["completed", "blocked", "cancelled"] {
        let f = Fixture::new();
        f.register();
        let coordination = execute_handoff_operation(
            "handoff.begin",
            json!({"request_id":"doctor-report", "deliverable":"Synthetic reviewed result"}),
            &f.runtime(),
        )
        .unwrap()["coordination_id"]
            .clone();
        let mut input = json!({"request_id":"doctor-resolution", "coordination_id":coordination,
            "outcome":outcome,"report":"Synthetic saved report", "reconciled_assignments":[]});
        if outcome != "completed" {
            input["reason"] = json!("Synthetic approval remains outstanding");
        }
        let resolved = execute_handoff_operation("handoff.resolve", input, &f.runtime()).unwrap();
        let db = rusqlite::Connection::open(f.root.join("namedrop.sqlite3")).unwrap();
        let attempt: String = db
            .query_row(
                "SELECT a.attempt_id FROM delivery_attempts a JOIN mailbox_deliveries d
             USING(session_socket_id,worktree_id,delivery_id,address_id)
             WHERE d.message_id=?1 ORDER BY a.rowid DESC LIMIT 1",
                [resolved["report_message_id"].as_str().unwrap()],
                |r| r.get(0),
            )
            .unwrap();
        drop(db);
        execute_integration_operation("complete", json!({"attempt_id":attempt}), &f.runtime())
            .unwrap();
        // Simulate an old version's persisted generic warning after transport settled.
        StateStore::open(&f.root)
            .unwrap()
            .transaction(|tx| {
                tx.open_attention_item(
                    &StateNamespace::for_space("socket-a", "workspace-1"),
                    &AttentionItemDraft {
                        id: AttentionItemId::new(format!(
                            "coordination-attention:{}",
                            coordination.as_str().unwrap()
                        )),
                        source_kind: AttentionSourceKind::Coordination,
                        source_id: coordination.as_str().unwrap().to_owned(),
                        summary: "Coordination report is saved. Presentation is unknown.".into(),
                    },
                    &MutationAttribution::new(ActorKind::Plugin, "old-version", 100),
                )
                .map(|_| ())
            })
            .unwrap();
        let before = fs::read(f.root.join("namedrop.sqlite3")).unwrap();
        assert!(
            diagnose_collaboration(&f.runtime())
                .issues
                .iter()
                .any(|i| i.code == "ND-COORDINATION-001")
        );
        assert_eq!(
            before,
            fs::read(f.root.join("namedrop.sqlite3")).unwrap(),
            "Doctor remains read-only"
        );
        drain_mailboxes(&f.runtime()).unwrap();
        let after_reconcile = fs::read(f.root.join("namedrop.sqlite3")).unwrap();
        let report = diagnose_collaboration(&f.runtime());
        let issues: Vec<_> = report
            .issues
            .iter()
            .filter(|i| i.code == "ND-COORDINATION-001")
            .collect();
        if outcome == "completed" {
            assert!(
                issues.is_empty(),
                "completed settled report must not need attention: {issues:?}"
            );
        } else {
            assert_eq!(issues.len(), 1);
            assert!(
                issues[0]
                    .explanation
                    .contains("Synthetic approval remains outstanding")
            );
            assert!(issues[0].explanation.contains(outcome));
        }
        assert_eq!(
            after_reconcile,
            fs::read(f.root.join("namedrop.sqlite3")).unwrap()
        );
        let saved = execute_handoff_operation(
            "handoff.results",
            json!({"coordination_id":coordination}),
            &f.runtime(),
        )
        .unwrap();
        assert_eq!(saved["presentation"], "unknown");
        assert_eq!(saved["report"], "Synthetic saved report");
        assert_eq!(saved["outcome"], outcome);
        assert_eq!(saved["report_delivery_state"], "settled");
    }
}

#[test]
fn doctor_flags_a_reinstalled_build_the_active_release_does_not_run() {
    let f = Fixture::new();
    f.register();
    let root = f.stage_runtime(b"old build", Some(b"old build\n"), true);
    let report = diagnose_collaboration(&f.runtime());
    let stale = stale_runtime(&report).expect("a build the active release does not run");
    assert_eq!(stale.severity, HealthSeverity::Notice);
    assert_eq!(stale.repair.kind, HealthRepairKind::ProductAction);
    assert_eq!(
        stale.repair.command,
        Some(vec![
            "sh".to_owned(),
            root.join("scripts/setup.sh").display().to_string(),
            "activate".to_owned(),
            "--human".to_owned(),
        ])
    );
    assert_eq!(
        stale.diagnostic["path"],
        json!(root.join("bin/namedrop").display().to_string())
    );
    assert!(stale.explanation.contains("Update Namedrop"));
}

#[test]
fn doctor_flags_a_same_size_rebuild_by_digest() {
    let f = Fixture::new();
    f.register();
    f.stage_runtime(b"old!", Some(b"new!"), true);
    assert!(stale_runtime(&diagnose_collaboration(&f.runtime())).is_some());
}

#[test]
fn doctor_stays_quiet_when_the_active_release_is_the_built_one() {
    let f = Fixture::new();
    f.register();
    let baseline = serde_json::to_value(diagnose_collaboration(&f.runtime())).unwrap();
    f.stage_runtime(b"same build", Some(b"same build"), true);
    assert_eq!(
        serde_json::to_value(diagnose_collaboration(&f.runtime())).unwrap(),
        baseline
    );
    assert_eq!(f.host.plugin_lists.get(), 1, "the check did read the root");
}

#[test]
fn doctor_ignores_a_root_without_a_staged_build() {
    let f = Fixture::new();
    f.register();
    let root = f.stage_runtime(b"old build", None, true);
    let target = root.join("target/aarch64-apple-darwin/release");
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("namedrop"), b"dev build").unwrap();
    assert!(stale_runtime(&diagnose_collaboration(&f.runtime())).is_none());
    assert_eq!(f.host.plugin_lists.get(), 1, "the check did read the root");
}

#[test]
fn doctor_skips_the_runtime_check_without_a_launcher() {
    let f = Fixture::new();
    f.register();
    f.stage_runtime(b"old build", Some(b"new build"), false);
    assert!(stale_runtime(&diagnose_collaboration(&f.runtime())).is_none());
    assert_eq!(
        f.host.plugin_lists.get(),
        0,
        "no Herdr read without an installed runtime"
    );
}

#[test]
fn doctor_skips_the_runtime_check_when_herdr_has_no_plugin_root() {
    let f = Fixture::new();
    f.register();
    f.stage_runtime(b"old build", Some(b"new build"), true);
    *f.host.plugin_root.borrow_mut() = None;
    let report = diagnose_collaboration(&f.runtime());
    assert!(stale_runtime(&report).is_none());
    assert_ne!(report.status, HealthStatus::Unavailable);
    assert_eq!(f.host.plugin_lists.get(), 1);
}

/// Two Codex panes: pane-1 holds the Fixture's session, pane-2 has none.
struct TwoCodexPanes<'a> {
    host: &'a Host,
    foreground: Option<u32>,
}
impl HerdrCommandExecutor for TwoCodexPanes<'_> {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        if let ["pane", "process-info", "--pane", _] = args {
            let pid = self.foreground.ok_or("herdr did not answer")?;
            return Ok(HerdrCommandOutput {
                stdout: json!({"result":{"process_info":{"shell_pid":1,"foreground_process_group_id":pid,"foreground_processes":[{"pid":pid}]}}}).to_string(),
            });
        }
        let mut output = self.host.execute(args)?;
        if args.starts_with(&["pane", "list"]) {
            let mut value: serde_json::Value = serde_json::from_str(&output.stdout).unwrap();
            let mut second = value["result"]["panes"][0].clone();
            second["pane_id"] = json!("pane-2");
            second["terminal_id"] = json!("terminal-2");
            second["agent_session"] = serde_json::Value::Null;
            value["result"]["panes"]
                .as_array_mut()
                .unwrap()
                .push(second);
            for pane in value["result"]["panes"].as_array_mut().unwrap() {
                pane["agent"] = json!("codex");
            }
            output.stdout = value.to_string();
        }
        Ok(output)
    }
}

impl Fixture {
    /// A registered space whose pane-1 now runs a session that is not the
    /// registered one, so without the server it reports ND-SESSION-001, and
    /// whose pane-2 has no session, so it reports ND-SESSION-002.
    fn two_codex_panes(&self, foreground: Option<u32>) -> TwoCodexPanes<'_> {
        self.register();
        *self.host.session.borrow_mut() = "session-new".into();
        TwoCodexPanes {
            host: &self.host,
            foreground,
        }
    }
    fn codex_runtime<'a>(
        &'a self,
        host: &'a TwoCodexPanes<'a>,
    ) -> LifecycleRuntime<'a, TwoCodexPanes<'a>, Time, Ids> {
        LifecycleRuntime {
            state_directory: &self.root,
            home_directory: Some(&self.home),
            agent_config: namedrop::AgentConfigDirectories::default(),
            cwd: Path::new("/repo"),
            workspace_id: Some("workspace-1"),
            current_pane_id: Some("pane-2"),
            session_socket_id: "socket-a",
            herdr: host,
            clock: &Time,
            ids: &self.ids,
        }
    }
}

fn issues_with<'a>(report: &'a HealthReport, code: &str) -> Vec<&'a HealthIssue> {
    report
        .issues
        .iter()
        .filter(|issue| issue.code == code)
        .collect()
}

#[test]
fn doctor_reports_a_shared_codex_app_server_once_with_only_its_remedy() {
    let f = Fixture::new();
    let codex_home = f.home.join(".codex");
    let server = codex_app_server::start_server(&codex_home);
    let host = f.two_codex_panes(None);
    let runtime = f.codex_runtime(&host);
    let codex_before = codex_app_server::snapshot(&codex_home);
    let database = fs::read(f.root.join("namedrop.sqlite3")).unwrap();

    let report = diagnose_collaboration(&runtime);

    let shared = issues_with(&report, "ND-SESSION-003");
    assert_eq!(shared.len(), 1, "one issue for the space, not one per pane");
    let value = serde_json::to_value(shared[0]).unwrap();
    assert_eq!(value["summary"], codex_app_server::SUMMARY);
    assert!(
        shared[0].explanation.contains(
            "Submitting another prompt or reinstalling the Herdr integration does not help."
        ),
        "{}",
        shared[0].explanation
    );
    assert!(
        !shared[0]
            .explanation
            .contains("daemon_auto_start is already off")
    );
    assert_eq!(shared[0].repair.kind, HealthRepairKind::HumanDecision);
    assert!(
        value["repair"]
            .get("command")
            .is_none_or(serde_json::Value::is_null),
        "the Doctor popup must have no command to run: {value}"
    );
    assert!(
        shared[0]
            .repair
            .prompt
            .contains("Namedrop never changes Codex settings")
    );
    let steps = value["diagnostic"]["steps"].as_array().unwrap();
    assert_eq!(steps.len(), 7);
    assert_eq!(
        [&steps[0], &steps[1], &steps[4], &steps[6]],
        codex_app_server::COMMANDS
            .map(|command| json!(command))
            .each_ref(),
        "each command is a step of its own"
    );
    assert!(
        steps[2]
            .as_str()
            .unwrap()
            .starts_with("Then restart Codex in each of its Herdr panes.")
    );
    let pid_file = codex_app_server::pid_file(&codex_home);
    assert_eq!(value["diagnostic"]["path"], json!(pid_file));
    assert_eq!(value["diagnostic"]["process_id"], server.pid());
    assert_eq!(value["diagnostic"]["pane_ids"], json!(["pane-1", "pane-2"]));
    assert_eq!(value["diagnostic"]["auto_start"], true);
    assert_eq!(value["diagnostic"]["runtime"], "codex");
    assert_eq!(value["diagnostic"]["workspace_id"], "workspace-1");

    let rendered = render_health_report_to_text(&report);
    assert!(
        rendered.contains(&format!(
            "Target: path={}, process_id={}, runtime=codex, workspace_id=workspace-1",
            pid_file.display(),
            server.pid()
        )),
        "{rendered}"
    );
    for command in codex_app_server::COMMANDS {
        assert!(
            rendered.contains(&format!("Step: {command}\n")),
            "{rendered}"
        );
    }

    assert!(
        issues_with(&report, "ND-SESSION-002").is_empty(),
        "a fresh-session remedy is wrong under the server: {:?}",
        report.issues
    );
    assert!(
        issues_with(&report, "ND-SESSION-001").is_empty(),
        "re-naming could bind the alias to another pane's session: {:?}",
        report.issues
    );
    assert_eq!(report.status, HealthStatus::NeedsAttention);
    assert_eq!(codex_app_server::snapshot(&codex_home), codex_before);
    assert_eq!(fs::read(f.root.join("namedrop.sqlite3")).unwrap(), database);
}

#[test]
fn doctor_names_a_server_that_outlived_turning_its_setting_off() {
    let f = Fixture::new();
    let codex_home = f.home.join(".codex");
    let _server = codex_app_server::start_server(&codex_home);
    fs::write(
        codex_home.join("config.toml"),
        "[features]\ndaemon_auto_start = false\n",
    )
    .unwrap();
    let host = f.two_codex_panes(None);
    let report = diagnose_collaboration(&f.codex_runtime(&host));
    let shared = issues_with(&report, "ND-SESSION-003");
    assert_eq!(shared.len(), 1);
    assert!(
        shared[0].explanation.ends_with(
            "daemon_auto_start is already off; the server started earlier keeps serving new Codex sessions until it is stopped."
        ),
        "{}",
        shared[0].explanation
    );
    assert_eq!(
        serde_json::to_value(shared[0]).unwrap()["diagnostic"]["auto_start"],
        false
    );
}

#[test]
fn doctor_keeps_the_session_issues_when_codex_runs_in_process() {
    let f = Fixture::new();
    let codex_home = f.home.join(".codex");
    fs::create_dir_all(&codex_home).unwrap();
    fs::write(
        codex_home.join("config.toml"),
        "[features]\ndaemon_auto_start = false\n",
    )
    .unwrap();
    let host = f.two_codex_panes(None);
    let report = diagnose_collaboration(&f.codex_runtime(&host));
    assert!(
        issues_with(&report, "ND-SESSION-003").is_empty(),
        "{:?}",
        report.issues
    );
    let missing = issues_with(&report, "ND-SESSION-002");
    assert_eq!(missing.len(), 1);
    assert_eq!(
        serde_json::to_value(missing[0]).unwrap()["diagnostic"]["pane_id"],
        "pane-2"
    );
    assert_eq!(issues_with(&report, "ND-SESSION-001").len(), 1);
}

#[test]
fn doctor_leaves_no_daemon_codex_panes_alone_beside_a_running_server() {
    let f = Fixture::new();
    let _server = codex_app_server::start_server(&f.home.join(".codex"));
    let in_process = codex_app_server::in_process_codex();
    let host = f.two_codex_panes(Some(in_process.pid()));
    let report = diagnose_collaboration(&f.codex_runtime(&host));
    assert!(
        issues_with(&report, "ND-SESSION-003").is_empty(),
        "{:?}",
        report.issues
    );
    assert_eq!(issues_with(&report, "ND-SESSION-002").len(), 1);
}

#[test]
fn doctor_assistance_names_the_shared_app_server_for_a_pane_without_a_session() {
    let f = Fixture::new();
    let _server = codex_app_server::start_server(&f.home.join(".codex"));
    let host = f.two_codex_panes(None);
    let refused = request_doctor_assistance(&f.codex_runtime(&host), "").unwrap_err();
    assert!(
        refused.starts_with(&format!("Nothing was sent: {}", codex_app_server::SUMMARY)),
        "{refused}"
    );
    assert!(f.host.prompts.borrow().is_empty());
}
