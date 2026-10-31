use super::*;
struct NoCalls;
impl crate::HerdrCommandExecutor for NoCalls {
    fn execute(&self, args: &[&str]) -> Result<crate::HerdrCommandOutput, String> {
        panic!("unexpected host call {args:?}")
    }
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
            package_directory(&hc_state.join("integrations"), agent)
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
fn native_codex_accepts_tui_and_two_hc_marketplaces_together() {
    let root =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/owner-review1-checks/native-codex");
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
        let context = PackageContext::new(&home, &state, hc);
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
        let prefix = format!(
            "{} --plugin-state {}",
            shell_single_quote(&executable.to_string_lossy()),
            shell_single_quote(&state.to_string_lossy())
        );
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
                .arg(&executable)
                .arg("--plugin-state")
                .arg(&state)
                .args(&operation);
            if context.hc {
                command.args(["--host", "hc", "--hc-terminal", "t1", "--hc-space", "space"]);
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
        54
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
    let context = PackageContext::new(&home, &state, true);
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
                assert!(
                    argv.contains(&format!("--hc-hook\n{}\n", context.marketplace)),
                    "{argv}"
                );
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
