use namedrop::{AgentRuntime, install_integration_package, operation_catalog};
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "namedrop-codex-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        Self(root)
    }
    fn binary(&self) -> PathBuf {
        self.0.join("checkout with spaces/namedrop")
    }
    fn state(&self) -> PathBuf {
        self.0.join("plugin-state")
    }
    fn rules(&self) -> PathBuf {
        self.0.join(".codex/rules/namedrop.rules")
    }
    fn install(&self) -> Result<PathBuf, String> {
        install_integration_package(&self.0, AgentRuntime::Codex, &self.binary(), &self.state())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn fresh_codex_install_allows_the_required_roster_lookup() {
    let f = Fixture::new();
    assert!(!f.rules().exists());
    f.install().unwrap();
    let rules = fs::read_to_string(f.rules()).unwrap();
    let pattern = serde_json::json!([f.binary(), "--plugin-state", f.state(), "address", "list"]);
    assert!(
        rules.contains(&format!(
            "prefix_rule(pattern={pattern}, decision=\"allow\""
        )),
        "fresh installation does not allow its required address list command: {rules}"
    );
}

#[test]
fn codex_install_populates_an_empty_policy_file() {
    let f = Fixture::new();
    fs::create_dir_all(f.rules().parent().unwrap()).unwrap();
    fs::write(f.rules(), "").unwrap();
    f.install().unwrap();
    let rules = fs::read_to_string(f.rules()).unwrap();
    assert!(rules.starts_with("# Managed by Namedrop:"));
    assert_eq!(rules.matches("prefix_rule(").count(), 18);
}

#[test]
fn codex_install_refreshes_only_its_owned_policy_and_preserves_user_configuration() {
    let f = Fixture::new();
    fs::create_dir_all(f.0.join(".codex/rules")).unwrap();
    let unrelated = f.0.join(".codex/rules/default.rules");
    fs::write(&unrelated, "# user rules\n").unwrap();
    fs::write(f.0.join(".codex/config.toml"), "# user configuration\n").unwrap();
    let package = f.install().unwrap();
    let original = fs::read_to_string(f.rules()).unwrap();
    assert_eq!(original.matches("prefix_rule(").count(), 18);
    let modified = fs::metadata(f.rules()).unwrap().modified().unwrap();
    f.install().unwrap();
    assert_eq!(
        fs::metadata(f.rules()).unwrap().modified().unwrap(),
        modified
    );
    let next = f.0.join("next/namedrop");
    install_integration_package(&f.0, AgentRuntime::Codex, &next, &f.state()).unwrap();
    let updated = fs::read_to_string(f.rules()).unwrap();
    assert!(!updated.contains(f.binary().to_str().unwrap()));
    assert!(updated.contains(next.to_str().unwrap()));
    assert_eq!(fs::read_to_string(unrelated).unwrap(), "# user rules\n");
    assert_eq!(
        fs::read_to_string(f.0.join(".codex/config.toml")).unwrap(),
        "# user configuration\n"
    );
    let skill = fs::read_to_string(package.join("SKILL.md")).unwrap();
    assert!(skill.contains("--plugin-state"));
    assert!(skill.contains("--input-json-file"));
    assert!(!skill.contains("allowed-tools:"));
}

#[test]
fn codex_policy_conflicts_are_refused_before_package_changes() {
    let f = Fixture::new();
    let package = f.install().unwrap();
    let before = fs::read(package.join("SKILL.md")).unwrap();
    fs::write(f.rules(), "# not owned by Namedrop\n").unwrap();
    let error = install_integration_package(
        &f.0,
        AgentRuntime::Codex,
        &f.0.join("new/namedrop"),
        &f.state(),
    )
    .unwrap_err();
    assert!(error.contains("not owned"), "{error}");
    assert_eq!(fs::read(package.join("SKILL.md")).unwrap(), before);
    fs::remove_file(f.rules()).unwrap();
    let target = f.0.join("untouched");
    fs::write(&target, "untouched").unwrap();
    std::os::unix::fs::symlink(&target, f.rules()).unwrap();
    assert!(f.install().is_err());
    assert_eq!(fs::read_to_string(target).unwrap(), "untouched");
}

#[test]
fn direct_cli_input_file_and_state_binding_cannot_be_overridden() {
    let f = Fixture::new();
    let executable = env!("CARGO_BIN_EXE_namedrop");
    let input = f.0.join("input.json");
    fs::write(&input, "{}").unwrap();
    let output = Command::new(executable)
        .args([
            "--plugin-state",
            f.state().to_str().unwrap(),
            "describe",
            "send",
            "--input-json-file",
            input.to_str().unwrap(),
        ])
        .env_remove("HERDR_PLUGIN_STATE_DIR")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let data: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(data["status"], "ok");
    for extra in [
        vec!["--plugin-state", "/wrong"],
        vec!["--plugin-state=/wrong"],
        vec!["--input-json", "{}"],
    ] {
        let result = Command::new(executable)
            .args([
                "--plugin-state",
                f.state().to_str().unwrap(),
                "describe",
                "--input-json-file",
                input.to_str().unwrap(),
            ])
            .args(extra)
            .output()
            .unwrap();
        assert!(
            !result.status.success(),
            "conflicting transport accepted: {result:?}"
        );
    }
    assert!(!f.state().exists(), "describe must remain stateless");
}

#[test]
fn cli_context_targets_the_bound_host_instead_of_inherited_environment() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let herdr = f.0.join("herdr");
    fs::write(
        &herdr,
        "#!/bin/sh\nprintf '%s\\n' \"socket=$HERDR_SOCKET_PATH args=$*\" >&2\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&herdr, fs::Permissions::from_mode(0o700)).unwrap();
    for timeout in ["", "1000"] {
        let output = Command::new(env!("CARGO_BIN_EXE_namedrop"))
            .args([
                "--plugin-state",
                f.state().to_str().unwrap(),
                "delivery",
                "inbox",
                "--herdr-socket",
                "bound-socket",
                "--herdr-workspace",
                "bound-workspace",
                "--herdr-pane",
                "bound-pane",
            ])
            .env("HERDR_BIN_PATH", &herdr)
            .env("HERDR_SOCKET_PATH", "wrong-socket")
            .env("HERDR_WORKSPACE_ID", "wrong-workspace")
            .env("HERDR_PLUGIN_STATE_DIR", f.0.join("wrong-state"))
            .env("NAMEDROP_HERDR_TIMEOUT_MS", timeout)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            report["error"]["message"],
            "socket=bound-socket args=worktree list --workspace bound-workspace --json"
        );
    }
}

#[test]
#[ignore = "requires an installed Codex CLI; runs no model or messages"]
fn codex_policy_checker_accepts_only_installed_collaboration_commands() {
    let f = Fixture::new();
    f.install().unwrap();
    let check = |args: Vec<String>| {
        let result = Command::new("codex")
            .args(["execpolicy", "check", "--rules"])
            .arg(f.rules())
            .arg("--")
            .args(args)
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        serde_json::from_slice::<serde_json::Value>(&result.stdout).unwrap()
    };
    let prefix = vec![
        f.binary().to_string_lossy().into_owned(),
        "--plugin-state".into(),
        f.state().to_string_lossy().into_owned(),
    ];
    for operation in &operation_catalog().operations {
        let mut args = prefix.clone();
        args.extend(operation.argv.clone());
        args.extend(["--input-json-file".into(), "/tmp/report.json".into()]);
        assert_eq!(check(args)["decision"], "allow", "{}", operation.token);
    }
    // Both hosts register with the installed prefix and explicit Herdr flags,
    // so the existing register rule covers this challenge.
    let mut registration = prefix.clone();
    registration.extend([
        "register".into(),
        "--herdr-socket".into(),
        "socket-a".into(),
        "--herdr-workspace".into(),
        "workspace-1".into(),
        "--herdr-pane".into(),
        "pane-1".into(),
        "--input-json".into(),
        "{}".into(),
    ]);
    assert_eq!(
        check(registration)["decision"],
        "allow",
        "registration bootstrap"
    );
    assert_ne!(
        check(vec![
            f.binary().to_string_lossy().into_owned(),
            "register".into(),
            "--input-json".into(),
            "{}".into()
        ])["decision"],
        "allow",
        "unbound registration"
    );
    let mut roster = prefix.clone();
    roster.extend(["address".into(), "list".into()]);
    assert_eq!(check(roster)["decision"], "allow", "address.list");
    for op in [
        vec!["purge"],
        vec!["deactivate"],
        vec!["delivery", "cancel-held"],
        vec!["delivery", "acknowledge"],
        vec!["address", "remove"],
        vec!["address", "create"],
        vec!["address", "persona"],
        vec!["group", "set"],
        vec!["group", "remove"],
        vec!["event", "pane-lifecycle"],
        vec!["space", "resume"],
    ] {
        let mut args = prefix.clone();
        args.extend(op.iter().map(|x| x.to_string()));
        assert_ne!(check(args)["decision"], "allow", "{op:?}");
    }
    let mut wrong_state = prefix.clone();
    wrong_state[2] = "/other/state".into();
    wrong_state.push("reply".into());
    assert_ne!(check(wrong_state)["decision"], "allow");
    assert_ne!(
        check(vec!["sh".into(), "-c".into(), "echo bad".into()])["decision"],
        "allow"
    );
}

#[test]
fn global_state_binding_rejects_duplicates_across_command_levels_for_both_hosts() {
    let cases = [
        vec![
            "--plugin-state",
            "/first",
            "--plugin-state=/second",
            "describe",
        ],
        vec![
            "--plugin-state=/first",
            "describe",
            "--plugin-state",
            "/second",
        ],
        vec![
            "describe",
            "--plugin-state",
            "/first",
            "--plugin-state=/second",
        ],
        vec![
            "--plugin-state",
            "/first",
            "handoff",
            "--plugin-state=/second",
            "status",
        ],
        vec![
            "handoff",
            "--plugin-state=/first",
            "status",
            "--plugin-state",
            "/second",
        ],
    ];
    let mut failures = Vec::new();
    for host in ["tui", "hc"] {
        for arguments in &cases {
            let output = Command::new(env!("CARGO_BIN_EXE_namedrop"))
                .env_clear()
                .args(["--host", host, "--json"])
                .args(arguments)
                .output()
                .unwrap();
            println!("{host} {arguments:?} => {:?}", output.status.code());
            if output.status.code() != Some(64) {
                failures.push(format!("{host} {arguments:?}"));
            }
        }
        let root = std::env::temp_dir().join(format!(
            "namedrop-state-binding-{}-{host}",
            std::process::id()
        ));
        let state = root.to_str().unwrap();
        for arguments in [
            vec![
                "--plugin-state".to_owned(),
                state.to_owned(),
                "describe".to_owned(),
            ],
            vec!["describe".to_owned(), format!("--plugin-state={state}")],
            vec![
                "describe".to_owned(),
                "--plugin-state".to_owned(),
                state.to_owned(),
                "--".to_owned(),
                "--plugin-state=literal-operation".to_owned(),
            ],
        ] {
            let output = Command::new(env!("CARGO_BIN_EXE_namedrop"))
                .env_clear()
                .args(["--host", host, "--json"])
                .args(&arguments)
                .output()
                .unwrap();
            let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(body["protocol_version"], "collab/v1");
            if arguments
                .last()
                .is_some_and(|arg| arg == "--plugin-state=literal-operation")
            {
                assert_eq!(output.status.code(), Some(65));
                assert!(
                    !body["error"]["message"]
                        .as_str()
                        .unwrap()
                        .contains("supplied more than once")
                );
            } else {
                assert!(output.status.success(), "{host} {arguments:?}: {output:?}");
            }
        }
        let _ = std::fs::remove_dir_all(root);
    }
    assert!(
        failures.is_empty(),
        "duplicate state bindings accepted: {}",
        failures.join("; ")
    );
}
