use super::*;
struct NoCalls;
impl crate::HerdrCommandExecutor for NoCalls {
    fn execute(&self, args: &[&str]) -> Result<crate::HerdrCommandOutput, String> {
        panic!("unexpected host call {args:?}")
    }
}

fn owner_review_root(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/root-boundary-checks")
        .join(format!(
            "{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    fs::create_dir_all(&root).unwrap();
    root
}

fn tree_snapshot(root: &Path) -> std::collections::BTreeMap<PathBuf, (u32, Option<Vec<u8>>)> {
    fn visit(
        root: &Path,
        path: &Path,
        entries: &mut std::collections::BTreeMap<PathBuf, (u32, Option<Vec<u8>>)>,
    ) {
        let metadata = fs::symlink_metadata(path).unwrap();
        let contents = metadata.is_file().then(|| fs::read(path).unwrap());
        entries.insert(
            path.strip_prefix(root).unwrap().to_owned(),
            (metadata.permissions().mode(), contents),
        );
        if metadata.is_dir() {
            for entry in fs::read_dir(path).unwrap() {
                visit(root, &entry.unwrap().path(), entries);
            }
        }
    }
    let mut entries = std::collections::BTreeMap::new();
    visit(root, root, &mut entries);
    entries
}

fn refresh_hc_packages(home: &Path, state: &Path) -> IntegrationPackageInstallation {
    let runtime = LifecycleRuntime {
        state_directory: state,
        home_directory: Some(home),
        agent_config: AgentConfigDirectories::default(),
        cwd: home,
        workspace_id: Some("space"),
        current_pane_id: None,
        session_socket_id: "hc:root-boundary-test",
        host: crate::HcHost::new(&NoCalls),
        clock: &crate::SystemClock,
        ids: &crate::ProcessIds::default(),
    };
    let discovered = DiscoveredWorktree {
        worktree_id: crate::WorktreeId::new("worktree"),
        workspace_id: "space".into(),
        worktree_attached: false,
        label: None,
        branch: None,
        panes: vec![],
    };
    StateStore::open(state)
        .unwrap()
        .transaction(|transaction| {
            transaction.ensure_namespace(
                &discovered.namespace(runtime.session_socket_id),
                &MutationAttribution::new(crate::ActorKind::Plugin, "root-boundary-test", 1),
            )
        })
        .unwrap();
    refresh_user_integration_packages(&runtime, &discovered).unwrap()
}

#[test]
fn hc_package_symlinks_cannot_refresh_tui_packages_or_permissions() {
    let root = owner_review_root("tui-isolation");
    let home = root.join("home");
    let tui_state = root.join("tui");
    let hc_state = root.join("hc");
    fs::create_dir_all(&hc_state).unwrap();
    let executable = PathBuf::from(crate::collaboration_executable().unwrap());
    for agent in [AgentRuntime::Codex, AgentRuntime::ClaudeCode] {
        install_integration_package(&home, agent, &executable, &tui_state).unwrap();
    }
    let codex_package = package_directory(&home, AgentRuntime::Codex);
    let manifest: Value =
        serde_json::from_slice(&fs::read(codex_package.join(CODEX_MANIFEST_FILE)).unwrap())
            .unwrap();
    let cache = home
        .join(".codex/plugins/cache/namedrop/namedrop")
        .join(manifest["version"].as_str().unwrap());
    for relative in CODEX_SNAPSHOT_FILES {
        let target = cache.join(relative);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(codex_package.join(relative), target).unwrap();
    }
    fs::write(home.join(".codex/config.toml"), format!(
        "[marketplaces.namedrop]\nsource_type = \"local\"\nsource = {:?}\n[plugins.\"namedrop@namedrop\"]\nenabled = true\n",
        codex_package.to_str().unwrap()
    )).unwrap();
    let tui_context = PackageContext::new(&home, false);
    assert!(codex_configuration_current(
        &tui_context,
        AgentConfigDirectories::default()
    ));
    for agent in [AgentRuntime::Codex, AgentRuntime::ClaudeCode] {
        std::os::unix::fs::symlink(
            package_directory(&home, agent),
            PackageContext::new(&home, true).directory(agent),
        )
        .unwrap();
    }
    let before = tree_snapshot(&home);

    let installation = refresh_hc_packages(&home, &hc_state);
    assert_eq!(
        [AgentRuntime::Codex, AgentRuntime::ClaudeCode].map(|agent| installation.failed(agent)),
        [true, true],
        "HC must refuse its symlinked integrations boundary for both agents"
    );
    let after = tree_snapshot(&home);
    assert_eq!(
        before.keys().collect::<Vec<_>>(),
        after.keys().collect::<Vec<_>>()
    );
    for (path, original) in before {
        assert!(
            after[&path] == original,
            "HC changed bytes or mode at {}",
            path.display()
        );
    }
    assert!(codex_configuration_current(
        &tui_context,
        AgentConfigDirectories::default()
    ));
    for agent in [AgentRuntime::Codex, AgentRuntime::ClaudeCode] {
        inspect_integration_package(
            Some(&home),
            agent,
            &tui_state,
            AgentConfigDirectories::default(),
            false,
        )
        .unwrap();
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn hc_inspection_rejects_package_symlinks_even_when_packages_match() {
    let root = owner_review_root("inspection");
    let home = root.join("home");
    let state = root.join("hc");
    let installation = refresh_hc_packages(&home, &state);
    for agent in [AgentRuntime::Codex, AgentRuntime::ClaudeCode] {
        assert!(!installation.failed(agent));
        inspect_integration_package(
            Some(&home),
            agent,
            &state,
            AgentConfigDirectories::default(),
            true,
        )
        .unwrap();
    }
    let moved = root.join("moved-packages");
    fs::create_dir_all(&moved).unwrap();
    for agent in [AgentRuntime::Codex, AgentRuntime::ClaudeCode] {
        let owned = PackageContext::new(&home, true).directory(agent);
        let target = moved.join(agent.as_str());
        fs::rename(&owned, &target).unwrap();
        std::os::unix::fs::symlink(&target, &owned).unwrap();
    }
    let before = tree_snapshot(&moved);
    let rejected = [AgentRuntime::Codex, AgentRuntime::ClaudeCode].map(|agent| {
        inspect_integration_package(
            Some(&home),
            agent,
            &state,
            AgentConfigDirectories::default(),
            true,
        )
        .is_err()
    });
    assert_eq!(
        rejected,
        [true, true],
        "inspection must reject the owned symlink boundary"
    );
    assert_eq!(tree_snapshot(&moved), before);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn hc_package_files_are_preserved_and_block_both_agents() {
    let root = owner_review_root("file-boundary");
    let home = root.join("home");
    let state = root.join("hc");
    fs::create_dir_all(&state).unwrap();
    fs::create_dir_all(&home).unwrap();
    let owned = [AgentRuntime::Codex, AgentRuntime::ClaudeCode]
        .map(|agent| PackageContext::new(&home, true).directory(agent));
    for path in &owned {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "user data").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o640)).unwrap();
    }
    let before = tree_snapshot(&home);
    let installation = refresh_hc_packages(&home, &state);
    for agent in [AgentRuntime::Codex, AgentRuntime::ClaudeCode] {
        assert!(installation.failed(agent));
        assert!(
            inspect_integration_package(
                Some(&home),
                agent,
                &state,
                AgentConfigDirectories::default(),
                true
            )
            .is_err()
        );
    }
    for path in &owned {
        assert_eq!(fs::read(path).unwrap(), b"user data");
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o640
        );
    }
    assert_eq!(tree_snapshot(&home), before);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn package_installation_accepts_explicit_home_and_state_symlinks() {
    let root = owner_review_root("trusted-bases");
    let home = root.join("home");
    let state_base = root.join("state");
    let state = state_base.join("session");
    let real_home = root.join("real-home");
    let real_state = root.join("real-state");
    fs::create_dir_all(&real_home).unwrap();
    fs::create_dir_all(real_state.join("session")).unwrap();
    std::os::unix::fs::symlink(&real_home, &home).unwrap();
    std::os::unix::fs::symlink(&real_state, &state_base).unwrap();
    let executable = PathBuf::from(crate::collaboration_executable().unwrap());
    for hc in [false, true] {
        let context = PackageContext::new(&home, hc);
        if hc {
            install_shell_command(&home, &executable, &state, true).unwrap();
        }
        for agent in [AgentRuntime::Codex, AgentRuntime::ClaudeCode] {
            install_integration_package_with_config(
                &context,
                agent,
                &executable,
                &state,
                AgentConfigDirectories::default(),
            )
            .unwrap();
            inspect_integration_package(
                Some(&home),
                agent,
                &state,
                AgentConfigDirectories::default(),
                hc,
            )
            .unwrap();
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn hc_refresh_does_not_change_tui_packages_and_owns_private_packages() {
    let root = std::env::temp_dir().join(format!("namedrop-owner-review-{}", std::process::id()));
    let home = root.join("home");
    let tui_state = root.join("tui");
    let hc_state = root.join("hc");
    fs::create_dir_all(&tui_state).unwrap();
    let executable = PathBuf::from(crate::collaboration_executable().unwrap());
    let packages = [AgentRuntime::Codex, AgentRuntime::ClaudeCode].map(|agent| {
        let package = install_integration_package(&home, agent, &executable, &tui_state).unwrap();
        let bytes = fs::read(package.join(INSTRUCTIONS_FILE)).unwrap();
        (agent, package, bytes)
    });
    let runtime = LifecycleRuntime {
        state_directory: &hc_state,
        home_directory: Some(&home),
        agent_config: AgentConfigDirectories::default(),
        cwd: &root,
        workspace_id: Some("space"),
        current_pane_id: None,
        session_socket_id: "hc:test",
        host: crate::HcHost::new(&NoCalls),
        clock: &crate::SystemClock,
        ids: &crate::ProcessIds::default(),
    };
    let discovered = DiscoveredWorktree {
        worktree_id: crate::WorktreeId::new("worktree"),
        workspace_id: "space".into(),
        worktree_attached: false,
        label: None,
        branch: None,
        panes: vec![],
    };
    let installation = refresh_user_integration_packages(&runtime, &discovered).unwrap();
    for (agent, package, bytes) in packages {
        assert!(!installation.failed(agent));
        assert_eq!(
            fs::read(package.join(INSTRUCTIONS_FILE)).unwrap(),
            bytes,
            "HC changed TUI package"
        );
        assert!(
            PackageContext::new(&home, true)
                .directory(agent)
                .join(INSTRUCTIONS_FILE)
                .exists()
        );
        inspect_integration_package(
            Some(&home),
            agent,
            &tui_state,
            AgentConfigDirectories::default(),
            false,
        )
        .unwrap();
    }
    fs::remove_dir_all(root).unwrap();
}
#[test]
#[ignore = "requires native Codex plugin CLI; isolated config, no model"]
fn native_codex_accepts_tui_and_one_stable_hc_marketplace_across_states() {
    let root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/owner-review1-checks/native-codex");
    let _ = fs::remove_dir_all(&root);
    let home = root.join("home");
    let codex = root.join("codex");
    let claude = root.join("claude");
    fs::create_dir_all(&codex).unwrap();
    let config = AgentConfigDirectories {
        claude: Some(&claude),
        codex: Some(&codex),
    };
    let executable = PathBuf::from(crate::collaboration_executable().unwrap());
    let contexts = [("tui", false), ("hc-one", true), ("hc-two", true)].map(|(name, hc)| {
        let state = root.join(name);
        fs::create_dir_all(&state).unwrap();
        let context = PackageContext::new(&home, hc);
        for runtime in [AgentRuntime::Codex, AgentRuntime::ClaudeCode] {
            install_integration_package_with_config(&context, runtime, &executable, &state, config)
                .unwrap();
        }
        context
    });
    for context in &contexts {
        let package = context.directory(AgentRuntime::Codex);
        for args in [
            vec![
                "plugin".to_owned(),
                "marketplace".to_owned(),
                "add".to_owned(),
                package.to_string_lossy().into_owned(),
                "--json".to_owned(),
            ],
            vec![
                "plugin".to_owned(),
                "add".to_owned(),
                context.plugin_key(),
                "--json".to_owned(),
            ],
        ] {
            let output = std::process::Command::new("codex")
                .args(&args)
                .env("HOME", &home)
                .env("CODEX_HOME", &codex)
                .env_remove("CODEX_SESSION_ID")
                .output()
                .unwrap();
            println!(
                "{args:?}: {}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                output.status.success(),
                "native Codex rejected simultaneous marketplace identities"
            );
        }
    }
    for context in &contexts {
        assert!(
            codex_configuration_current(context, config),
            "{} must remain current",
            context.marketplace
        );
        assert!(
            codex
                .join("rules")
                .join(format!("{}.rules", context.marketplace))
                .exists()
        );
        let guidance = hook_activation_guidance(Some(context), config);
        assert_eq!(guidance["codex"]["cache_current"], true);
    }
    for (context, state_name) in contexts.iter().zip(["tui", "hc-one", "hc-two"]) {
        let state = root.join(state_name);
        let instructions = fs::read_to_string(
            context
                .directory(AgentRuntime::Codex)
                .join(INSTRUCTIONS_FILE),
        )
        .unwrap();
        let prefix = if context.hc {
            shell_single_quote(&context.command(&executable).to_string_lossy())
        } else {
            format!(
                "{} --plugin-state {}",
                shell_single_quote(&executable.to_string_lossy()),
                shell_single_quote(&state.to_string_lossy())
            )
        };
        assert!(instructions.contains(&format!("Use this exact installed command prefix whenever these instructions name `namedrop`: `{prefix}`.")));
        for operation in collaboration_permission_operations() {
            let mut command = std::process::Command::new("codex");
            command
                .env("HOME", &home)
                .env("CODEX_HOME", &codex)
                .args(["execpolicy", "check", "--rules"])
                .arg(
                    codex
                        .join("rules")
                        .join(format!("{}.rules", context.marketplace)),
                )
                .arg("--")
                .arg(context.command(&executable));
            if context.hc {
                command
                    .args(&operation)
                    .arg("--plugin-state")
                    .arg(&state)
                    .args(["--host", "hc", "--hc-terminal", "t1", "--hc-space", "space"]);
            } else {
                command.arg("--plugin-state").arg(&state).args(&operation);
            }
            let output = command.output().unwrap();
            assert!(output.status.success(), "{output:?}");
            let result: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(
                result["decision"], "allow",
                "{} {operation:?}",
                context.marketplace
            );
        }
    }
    for arguments in [
        vec!["activate"],
        vec!["--plugin-state", "/state", "register"],
        vec![],
    ] {
        let output = std::process::Command::new("codex")
            .env("HOME", &home)
            .env("CODEX_HOME", &codex)
            .args(["execpolicy", "check", "--rules"])
            .arg(codex.join("rules/namedrop-app.rules"))
            .arg("--")
            .arg(home.join(".local/bin/namedrop-app"))
            .args(&arguments)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_ne!(
            result["decision"], "allow",
            "HC policy granted a broad command prefix: {arguments:?}"
        );
    }
    for (context, state_name) in contexts.iter().zip(["tui", "hc-one", "hc-two"]).rev() {
        for runtime in [AgentRuntime::Codex, AgentRuntime::ClaudeCode] {
            install_integration_package_with_config(
                context,
                runtime,
                &executable,
                &root.join(state_name),
                config,
            )
            .unwrap();
        }
    }
    for context in &contexts {
        assert!(codex_configuration_current(context, config));
    }
    let discovery = std::process::Command::new("python3")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/codex_discovery.py"))
        .arg(&root)
        .output()
        .unwrap();
    println!("{}", String::from_utf8_lossy(&discovery.stdout));
    assert!(
        discovery.status.success(),
        "native discovery failed: {}",
        String::from_utf8_lossy(&discovery.stderr)
    );
    let settings: Value =
        serde_json::from_slice(&fs::read(claude.join("settings.json")).unwrap()).unwrap();
    assert_eq!(
        settings["permissions"]["allow"].as_array().unwrap().len(),
        36
    );
    assert!(!home.join(".codex").exists());
    assert!(!home.join(".claude/settings.json").exists());
}

#[test]
fn hc_wrappers_derive_runtime_state_without_claiming_the_inherited_terminal() {
    let root = std::env::temp_dir().join(format!("namedrop-hc-wrapper-{}", std::process::id()));
    let home = root.join("home");
    let state = root.join("xdg/namedrop/hc/session");
    fs::create_dir_all(&state).unwrap();
    let executable = root.join("fake-namedrop");
    let log = root.join("argv");
    fs::write(
        &executable,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n",
            shell_single_quote(log.to_str().unwrap())
        ),
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let context = PackageContext::new(&home, true);
    install_shell_command(&home, &executable, &state, true).unwrap();
    for runtime in [AgentRuntime::ClaudeCode, AgentRuntime::Codex] {
        let package = install_integration_package_with_config(
            &context,
            runtime,
            &executable,
            &state,
            AgentConfigDirectories::default(),
        )
        .unwrap();
        for script in [HOOK_SCRIPT_FILE, SESSION_START_SCRIPT_FILE] {
            for override_state in [None, Some("/override-state")] {
                let mut command = std::process::Command::new(package.join(script));
                command
                    .env("HOME", &home)
                    .env("HC_TERMINAL_ID", "inherited-t3")
                    .env("XDG_STATE_HOME", root.join("xdg"))
                    .env("HC_SESSION", "session")
                    .env_remove("HERDR_SOCKET_PATH")
                    .env_remove("HERDR_PANE_ID");
                if let Some(state) = override_state {
                    command.env("NAMEDROP_STATE_DIR", state);
                } else {
                    command.env_remove("NAMEDROP_STATE_DIR");
                }
                assert!(command.output().unwrap().status.success());
                let argv = fs::read_to_string(&log).unwrap();
                assert!(argv.contains("--hc-hook\nhook\n"), "{argv}");
                assert!(!argv.contains("--hc-terminal"), "{argv}");
                assert!(!argv.contains("inherited-t3"), "{argv}");
                assert!(
                    argv.contains(&format!(
                        "--state-directory\n{}\n",
                        override_state.unwrap_or(state.to_str().unwrap())
                    )),
                    "{argv}"
                );
            }
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn tui_cache_inspection_ignores_unowned_nested_skill_files() {
    let root = std::env::temp_dir().join(format!("namedrop-snapshot-owner-{}", std::process::id()));
    let installed = root.join("installed");
    let cached = root.join("cached");
    for directory in [&installed, &cached] {
        for relative in CODEX_SNAPSHOT_FILES {
            let path = directory.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "same owned content").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    fs::create_dir_all(installed.join("skills/namedrop")).unwrap();
    fs::write(
        installed.join("skills/namedrop/SKILL.md"),
        "unowned TUI file",
    )
    .unwrap();
    assert!(codex_snapshot_matches(&installed, &cached));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn hc_addendum_package_is_stable_across_states_and_executables() {
    let root =
        std::env::temp_dir().join(format!("namedrop-addendum-stable-{}", std::process::id()));
    let home = root.join("home");
    let first_state = root.join("session-one");
    let second_state = root.join("session-two");
    let first = PackageContext::new(&home, true);
    let second = PackageContext::new(&home, true);
    assert_eq!(
        first.directory(AgentRuntime::ClaudeCode),
        home.join(".claude/skills/namedrop-app")
    );
    assert_eq!(
        first.directory(AgentRuntime::Codex),
        home.join(".agents/skills/namedrop-app")
    );
    assert_eq!(first.marketplace, "namedrop-app");
    for agent in [AgentRuntime::ClaudeCode, AgentRuntime::Codex] {
        let one = install_integration_package_with_config(
            &first,
            agent,
            Path::new("/development/one"),
            &first_state,
            AgentConfigDirectories::default(),
        )
        .unwrap();
        let snapshot = tree_snapshot(&one);
        let rules = home.join(".codex/rules/namedrop-app.rules");
        let policy = fs::read(&rules).ok();
        let two = install_integration_package_with_config(
            &second,
            agent,
            Path::new("/development/two"),
            &second_state,
            AgentConfigDirectories::default(),
        )
        .unwrap();
        assert_eq!(one, two);
        assert_eq!(
            tree_snapshot(&two),
            snapshot,
            "installed package bytes and modes must stay stable"
        );
        assert_eq!(fs::read(&rules).ok(), policy);
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn hc_addendum_activation_writes_only_its_own_bootstrap() {
    let root =
        std::env::temp_dir().join(format!("namedrop-addendum-launcher-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join(".local/bin")).unwrap();
    fs::write(
        root.join(".local/bin/namedrop"),
        "#!/bin/sh\n# Managed by Namedrop: shell command.\nexit 41\n",
    )
    .unwrap();
    let before = fs::read(root.join(".local/bin/namedrop")).unwrap();
    install_shell_command(
        &root,
        Path::new("/development/namedrop"),
        &root.join("state"),
        true,
    )
    .unwrap();
    assert_eq!(fs::read(root.join(".local/bin/namedrop")).unwrap(), before);
    assert!(root.join(".local/bin/namedrop-app").is_file());
    assert!(!root.join(".bashrc").exists());
    assert!(!root.join(".profile").exists());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn hc_permission_ownership_survives_state_changes_and_deactivation() {
    let root = owner_review_root("permission-owner");
    let home = root.join("home");
    let first = root.join("session-one");
    let second = root.join("session-two");
    let context = PackageContext::new(&home, true);
    let executable = root.join("first-executable");
    install_integration_package_with_config(
        &context,
        AgentRuntime::ClaudeCode,
        &executable,
        &first,
        AgentConfigDirectories::default(),
    )
    .unwrap();
    let settings = home.join(".claude/settings.json");
    let journal = home.join(".local/state/namedrop-app/claude-permissions.json");
    let expected = fs::read(&settings).unwrap();
    let ownership = fs::read(&journal).unwrap();
    install_integration_package_with_config(
        &PackageContext::new(&home, true),
        AgentRuntime::ClaudeCode,
        &root.join("other-executable"),
        &second,
        AgentConfigDirectories::default(),
    )
    .unwrap();
    assert_eq!(fs::read(&settings).unwrap(), expected);
    assert_eq!(fs::read(&journal).unwrap(), ownership);
    assert_eq!(remove_claude_permissions(&first).unwrap(), 0);
    assert_eq!(remove_claude_permissions(&second).unwrap(), 0);
    assert_eq!(fs::read(&settings).unwrap(), expected);
    assert_eq!(fs::read(&journal).unwrap(), ownership);
    assert!(!first.join("claude-permissions.json").exists());
    assert!(!second.join("claude-permissions.json").exists());
    fs::remove_dir_all(root).unwrap();
}

use crate::bash_rules;

#[test]
fn hc_command_forms_match_only_operation_scoped_permissions() {
    let root = owner_review_root("hc-operation-rules");
    let home = root.join("home with spaces");
    let state = root.join("session state");
    let context = PackageContext::new(&home, true);
    let executable = crate::integration::tool_executable(Some(&home), true).unwrap();
    install_integration_package_with_config(
        &context,
        AgentRuntime::ClaudeCode,
        Path::new("/development/namedrop"),
        &state,
        AgentConfigDirectories::default(),
    )
    .unwrap();
    let settings: Value =
        serde_json::from_slice(&fs::read(home.join(".claude/settings.json")).unwrap()).unwrap();
    let allowed = |command: &str| {
        settings["permissions"]["allow"]
            .as_array()
            .unwrap()
            .iter()
            .any(|rule| bash_rules::bash_rule_matches(rule.as_str().unwrap(), command))
    };
    let catalog = crate::operation_catalog();
    let operations = catalog.operations.iter().chain(
        catalog
            .management_operations
            .iter()
            .filter(|op| op.automatic_permission),
    );
    let tool = json!({
        "executable": executable,
        "environment": crate::integration::tool_environment(&state, "hc:session", "space", "terminal"),
        "input_flag": "--input-json",
        "operations": operations.map(|op| json!({"token":op.token, "argv":op.argv, "input":{}})).collect::<Vec<_>>()
    });
    let guidance = crate::integration::delivery_command_guidance(&tool).unwrap();
    for line in guidance.lines().skip(1) {
        let (_, command) = line.split_once(": ").unwrap();
        assert!(
            allowed(command),
            "HC guidance is outside managed policy: {command}"
        );
        assert!(command.find("--plugin-state").unwrap() > command.find("namedrop-app'").unwrap());
    }
    let registration = crate::integration::registration_command(
        &state,
        "hc:session",
        "space",
        "terminal",
        Path::new(&executable),
        AgentRuntime::ClaudeCode,
        "nonce",
    )
    .unwrap();
    assert!(allowed(&registration), "{registration}");
    for operation in [
        "activate",
        "deactivate",
        "name other",
        "--plugin-state /state register",
        "snapshot",
    ] {
        assert!(
            !allowed(&format!("{} {operation}", shell_single_quote(&executable))),
            "broad HC permission for {operation}"
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn hc_bootstrap_stale_runtime_only_inspects_the_shared_command() {
    const CHILD_ROOT: &str = "NAMEDROP_BOOTSTRAP_STALE_TEST_ROOT";
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        let root = PathBuf::from(root);
        let home = root.join("home");
        let state = root.join("state");
        let executable = PathBuf::from(crate::collaboration_executable().unwrap());
        let command = home.join(".local/bin/namedrop-app");
        for damage in [
            "intact",
            "deleted",
            "corrupt",
            "nonexecutable",
            "unowned",
            "symlink",
        ] {
            let _ = fs::remove_file(&command);
            install_shell_command(&home, &executable, &state, true).unwrap();
            match damage {
                "deleted" => fs::remove_file(&command).unwrap(),
                "corrupt" => fs::write(
                    &command,
                    "#!/bin/sh\n# Managed by Namedrop: shell command.\nexit 41\n",
                )
                .unwrap(),
                "nonexecutable" => {
                    fs::set_permissions(&command, fs::Permissions::from_mode(0o644)).unwrap()
                }
                "unowned" => fs::write(&command, "#!/bin/sh\nexit 41\n").unwrap(),
                "symlink" => {
                    fs::rename(&command, root.join("outside-command")).unwrap();
                    std::os::unix::fs::symlink(root.join("outside-command"), &command).unwrap();
                }
                _ => {}
            }
            let before = fs::read(&command).ok();
            let mode = fs::symlink_metadata(&command)
                .ok()
                .map(|m| m.permissions().mode());
            let installation = refresh_hc_packages(&home, &state);
            for agent in [AgentRuntime::Codex, AgentRuntime::ClaudeCode] {
                assert_eq!(
                    installation.failed(agent),
                    damage != "intact",
                    "stale runtime accepted {damage} bootstrap"
                );
            }
            assert_eq!(
                fs::read(&command).ok(),
                before,
                "stale runtime changed {damage} bootstrap"
            );
            assert_eq!(
                fs::symlink_metadata(&command)
                    .ok()
                    .map(|m| m.permissions().mode()),
                mode
            );
            assert!(
                !home.join(".agents").exists(),
                "stale runtime installed packages"
            );
        }
        return;
    }
    let root = owner_review_root("bootstrap-stale-runtime");
    let runtime = root.join("runtime");
    let old = runtime.join("releases").join("a".repeat(64));
    let current = runtime.join("releases").join("b".repeat(64));
    fs::create_dir_all(&old).unwrap();
    fs::create_dir_all(&current).unwrap();
    fs::hard_link(std::env::current_exe().unwrap(), old.join("namedrop")).unwrap();
    fs::write(old.join("runtime.json"), include_str!("../../runtime.json")).unwrap();
    fs::write(current.join("namedrop"), "active runtime").unwrap();
    fs::write(runtime.join("namedrop"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(runtime.join("namedrop"), fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink(
        Path::new("releases").join("b".repeat(64)),
        runtime.join("current"),
    )
    .unwrap();
    let output = std::process::Command::new(old.join("namedrop"))
        .args(["--exact", "install::owner_review_tests::hc_bootstrap_stale_runtime_only_inspects_the_shared_command", "--nocapture"])
        .env(CHILD_ROOT, &root)
        .output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn hc_bootstrap_inspection_accepts_another_states_activated_executable() {
    let root = owner_review_root("bootstrap-portability");
    let home = root.join("home");
    let first = root.join("first-state");
    let second = root.join("second-state");
    let installation = refresh_hc_packages(&home, &first);
    for agent in [AgentRuntime::Codex, AgentRuntime::ClaudeCode] {
        assert!(!installation.failed(agent));
    }
    let executable = root.join("other executable with ' quote");
    fs::write(&executable, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    install_shell_command(&home, &executable, &second, true).unwrap();
    let before = tree_snapshot(&home);
    for state in [&first, &second] {
        for agent in [AgentRuntime::Codex, AgentRuntime::ClaudeCode] {
            inspect_integration_package(
                Some(&home),
                agent,
                state,
                AgentConfigDirectories::default(),
                true,
            )
            .unwrap();
        }
    }
    assert_eq!(tree_snapshot(&home), before);
    fs::remove_dir_all(root).unwrap();
}
