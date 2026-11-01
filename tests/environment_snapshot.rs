#[path = "support/environment.rs"]
mod environment;
#[path = "support/socket_fixture.rs"]
mod socket_fixture;
use environment::CommandHome;
use namedrop::{cli::RuntimeOptions, runtime::HostKind};
use std::{fs, process::Command};

#[test]
fn runtime_roots_stay_at_the_construction_environment() {
    if std::env::var_os("NAMEDROP_SNAPSHOT_CHILD").is_none() {
        let root = socket_fixture::short_socket_root();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "runtime_roots_stay_at_the_construction_environment",
                "--nocapture",
            ])
            .isolated_home(&root)
            .env_remove("NAMEDROP_STATE_DIR")
            .env_remove("HC_SOCKET")
            .env_remove("HC_TERMINAL_SOCKET")
            .env_remove("HERDR_CONFIG_PATH")
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME")
            .env("HC_BIN_PATH", "/bin/true")
            .env("HC_SESSION", "default")
            .env("NAMEDROP_SNAPSHOT_CHILD", "1")
            .output()
            .unwrap();
        fs::remove_dir_all(&root).unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let original = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    let endpoint = original.join(".runtime/herdr-core/default.sock");
    fs::create_dir_all(endpoint.parent().unwrap()).unwrap();
    socket_fixture::assert_socket_path(&endpoint);
    let _listener = std::os::unix::net::UnixListener::bind(&endpoint).unwrap();
    let options = RuntimeOptions {
        host: Some(HostKind::Hc),
        ..Default::default()
    };
    let transport = namedrop::ProcessHerdr::from_snapshot(HostKind::Hc, &options.environment);
    // This child runs exactly one test and starts no threads.
    unsafe {
        for key in [
            "HOME",
            "XDG_STATE_HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_RUNTIME_DIR",
            "CLAUDE_CONFIG_DIR",
            "CODEX_HOME",
            "HC_SOCKET",
        ] {
            std::env::set_var(key, original.join("late").join(key));
        }
    }
    let snapshot = &options.environment;
    let config = snapshot.agent_config();
    let state = options.state_directory().unwrap();
    assert_eq!(state, original.join(".local/state/namedrop/hc/default"));
    assert_eq!(snapshot.home(), Some(original.as_path()));
    assert_eq!(
        config.state_root,
        Some(original.join(".local/state").as_path())
    );
    assert_eq!(config.config_root, Some(original.join(".config").as_path()));
    assert_eq!(
        config.data_root,
        Some(original.join(".local/share").as_path())
    );
    assert_eq!(
        config.runtime_root,
        Some(original.join(".runtime").as_path())
    );
    assert!(config.claude.is_none());
    assert!(config.codex.is_none());
    assert_eq!(snapshot.hc_endpoint().unwrap(), endpoint);
    let command = transport.checked_command().unwrap();
    for (key, expected) in [
        ("HC_SOCKET", &endpoint),
        ("HOME", &original),
        ("XDG_DATA_HOME", &original.join(".local/share")),
    ] {
        assert!(
            command
                .get_envs()
                .any(|(name, value)| name == key && value == Some(expected.as_os_str())),
            "captured {key} missing"
        );
    }
    let shortcuts = config.shortcut_config(snapshot.home()).unwrap();
    let report = namedrop::install_shortcuts_at(Some(&shortcuts)).unwrap();
    assert_eq!(
        report["path"],
        original.join(".config/herdr/config.toml").to_str().unwrap()
    );
    let settings = config.update_settings(snapshot.home()).unwrap();
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(&settings, r#"{"check_for_updates":false}"#).unwrap();
    fs::create_dir_all(&state).unwrap();
    fs::write(state.join("update-check.json"), r#"{"schema_version":1,"checked_at":1,"remote_version":"999.0.0","last_success_at":1,"last_error":null}"#).unwrap();
    assert!(
        namedrop::update_check::session_notice_at(
            r#"{"hook_event_name":"SessionStart"}"#,
            &state,
            1,
            None,
            Some(&settings)
        )
        .is_none()
    );
    assert!(!state.join("update-notice.json").exists());
    assert!(!original.join("late").exists());
}

#[test]
fn snapshot_keeps_non_unicode_roots_and_redacts_unrelated_credentials() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt, path::PathBuf};
    let root = PathBuf::from(OsString::from_vec(b"/snapshot-\xff".to_vec()));
    let snapshot = namedrop::environment::EnvironmentSnapshot::from_variables(
        [
            "HOME",
            "XDG_STATE_HOME",
            "XDG_CONFIG_HOME",
            "XDG_DATA_HOME",
            "XDG_RUNTIME_DIR",
            "HC_SOCKET",
        ]
        .into_iter()
        .map(|key| (key.into(), root.as_os_str().to_owned()))
        .chain(std::iter::once((
            "PRIVATE_TOKEN".into(),
            "secret-sentinel".into(),
        ))),
    );
    assert_eq!(snapshot.home(), Some(root.as_path()));
    assert_eq!(snapshot.agent_config().data_root, Some(root.as_path()));
    assert_eq!(snapshot.hc_endpoint().unwrap(), root);
    assert_eq!(
        snapshot.hc_state_directory().unwrap(),
        root.join("namedrop/hc/default")
    );
    assert!(!format!("{snapshot:?}").contains("secret-sentinel"));
    assert!(!format!("{:?}", snapshot.command("/bin/true")).contains("secret-sentinel"));
}

#[test]
fn hc_config_is_read_only_when_hc_is_selected_and_then_frozen() {
    use namedrop::{ProcessHerdr, environment::EnvironmentSnapshot};
    let root = std::env::temp_dir().join(format!("namedrop-lazy-socket-{}", std::process::id()));
    let path = root.join("herdr-core/config.toml");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    // A directory cannot be read as config. TUI construction must never try it.
    fs::create_dir(&path).unwrap();
    let snapshot = EnvironmentSnapshot::from_variables([
        ("XDG_CONFIG_HOME".into(), root.as_os_str().to_owned()),
        (
            "XDG_RUNTIME_DIR".into(),
            root.join("runtime").into_os_string(),
        ),
    ]);
    let _ = ProcessHerdr::from_snapshot(HostKind::Tui, &snapshot).command();
    fs::remove_dir(&path).unwrap();
    fs::write(&path, "socket = '/captured/config.sock'\n").unwrap();
    let transport = ProcessHerdr::from_snapshot(HostKind::Hc, &snapshot);
    fs::write(&path, "socket = '/late/config.sock'\n").unwrap();
    let command = transport.command();
    let child = snapshot.command("/bin/true");
    assert!(child.get_envs().any(|(key, value)| key == "HC_SOCKET"
        && value == Some(std::ffi::OsStr::new("/captured/config.sock"))));
    assert!(command.get_envs().any(|(key, value)| key == "HC_SOCKET"
        && value == Some(std::ffi::OsStr::new("/captured/config.sock"))));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn hc_update_and_shortcuts_share_resolved_config_roots() {
    use namedrop::{ProcessHerdr, environment::EnvironmentSnapshot};
    for (label, config_value) in [("empty", ""), ("relative", "relative-config")] {
        let root = std::env::temp_dir().join(format!(
            "namedrop-resolved-config-{}-{label}",
            std::process::id()
        ));
        let home = root.join("home");
        let config = home.join(".config");
        let endpoint = root.join("selected.sock");
        let hc_config = config.join("herdr-core/config.toml");
        fs::create_dir_all(hc_config.parent().unwrap()).unwrap();
        fs::write(
            &hc_config,
            format!("socket = {:?}\n", endpoint.to_str().unwrap()),
        )
        .unwrap();
        let snapshot = EnvironmentSnapshot::from_variables([
            ("HOME".into(), home.as_os_str().to_owned()),
            ("XDG_CONFIG_HOME".into(), config_value.into()),
            (
                "XDG_RUNTIME_DIR".into(),
                root.join("runtime").into_os_string(),
            ),
        ]);
        let paths = snapshot.agent_config();
        assert_eq!(paths.config_root, Some(config.as_path()));
        assert_eq!(
            paths.shortcut_config(snapshot.home()).unwrap(),
            config.join("herdr/config.toml")
        );
        assert_eq!(
            paths.update_settings(snapshot.home()).unwrap(),
            config.join("herdr/plugins/config/namedrop/settings.json")
        );
        let transport = ProcessHerdr::from_snapshot(HostKind::Hc, &snapshot);
        assert_eq!(
            snapshot.hc_endpoint().unwrap(),
            endpoint,
            "{label} XDG_CONFIG_HOME"
        );
        assert!(
            transport
                .command()
                .get_envs()
                .any(|(key, value)| key == "HC_SOCKET" && value == Some(endpoint.as_os_str()))
        );
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn hc_children_scrub_herdr_names_added_after_capture() {
    const MARKER: &str = "NAMEDROP_HC_SNAPSHOT_TEST_CHILD";
    if std::env::var_os(MARKER).is_none() {
        let home = std::env::temp_dir().join(format!("namedrop-hc-scrub-{}", std::process::id()));
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "hc_children_scrub_herdr_names_added_after_capture",
                "--nocapture",
            ])
            .isolated_home(&home)
            .env_remove("HERDR_PLUGIN_EVENT_JSON")
            .env_remove("HERDR_SESSION_ID")
            .env("HERDR_CONFIG_PATH", home.join("captured-config.toml"))
            .env("HC_SOCKET", home.join("fake.sock"))
            .env("HC_BIN_PATH", "/bin/sh")
            .env(MARKER, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let snapshot = namedrop::environment::EnvironmentSnapshot::capture();
    // This process runs only this test, with no concurrent environment readers.
    unsafe {
        std::env::set_var("HERDR_PLUGIN_EVENT_JSON", "late-event");
        std::env::set_var("HERDR_SESSION_ID", "late-session");
        std::env::remove_var("HERDR_CONFIG_PATH");
    }
    let transport = namedrop::ProcessHerdr::from_snapshot(HostKind::Hc, &snapshot);
    let output = transport.command()
        .args(["-c", "printf '%s\\n' \"${HERDR_PLUGIN_EVENT_JSON-unset}\" \"${HERDR_SESSION_ID-unset}\" \"${HERDR_CONFIG_PATH-unset}\""])
        .output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "unset\nunset\nunset\n"
    );
}

#[test]
fn hc_children_drop_captured_and_late_lifetime_tokens() {
    const CHILD: &str = "NAMEDROP_LIFETIME_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let root = socket_fixture::short_socket_root();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "hc_children_drop_captured_and_late_lifetime_tokens",
                "--nocapture",
            ])
            .isolated_env(&root)
            .env(CHILD, "1")
            .env("HC_SCOPE", "captured-scope")
            .env("HC_RUNTIME", "captured-runtime")
            .env("HC_TERMINAL_CUSTOM", "captured-terminal")
            .env("HERDR_CUSTOM", "captured-tui")
            .env("HC_SESSION", "kept-session")
            .env("HC_SOCKET", root.join("core.sock"))
            .env("HC_BIN_PATH", "/usr/bin/python3")
            .env("UNRELATED", "kept")
            .output()
            .unwrap();
        fs::remove_dir_all(root).unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let snapshot = namedrop::environment::EnvironmentSnapshot::capture();
    // SAFETY: this child runs one test and has no concurrent environment readers.
    unsafe {
        std::env::set_var("HC_TERMINAL_LATE", "late-terminal");
        std::env::set_var("HERDR_LATE", "late-tui");
    }
    let output = namedrop::ProcessHerdr::from_snapshot(HostKind::Hc, &snapshot)
        .command()
        .args([
            "-c",
            "import json,os,sys; print(json.dumps({'env':dict(os.environ),'argv':sys.argv[1:]}))",
            "events",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let env = result["env"].as_object().unwrap();
    let leaked: Vec<_> = env
        .keys()
        .filter(|key| {
            key.starts_with("HERDR_")
                || key.starts_with("HC_TERMINAL_")
                || matches!(key.as_str(), "HC_SCOPE" | "HC_RUNTIME")
        })
        .collect();
    assert!(
        leaked.is_empty(),
        "child inherited lifetime tokens: {leaked:?}"
    );
    assert_eq!(env["HC_SESSION"], "kept-session");
    assert_eq!(
        env["HC_SOCKET"],
        std::path::PathBuf::from(std::env::var_os("HOME").unwrap())
            .join("core.sock")
            .to_str()
            .unwrap()
    );
    assert_eq!(env["UNRELATED"], "kept");
    assert_eq!(result["argv"], serde_json::json!(["events", "--json"]));
}

#[test]
fn tui_children_drop_all_captured_and_late_hc_keys() {
    const CHILD: &str = "NAMEDROP_TUI_SCRUB_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let root = socket_fixture::short_socket_root();
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tui_children_drop_all_captured_and_late_hc_keys",
                "--nocapture",
            ])
            .isolated_env(&root)
            .env(CHILD, "1")
            .env("HERDR_BIN_PATH", "/usr/bin/python3")
            .env("HERDR_SOCKET_PATH", "/tui.sock")
            .env("HC_SOCKET", root.join("core.sock"))
            .env("HC_SESSION", "captured")
            .env("HC_TERMINAL_ID", "terminal")
            .env("HC_SCOPE", "scope")
            .env("HC_CUSTOM", "custom")
            .output()
            .unwrap();
        fs::remove_dir_all(root).unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let snapshot = namedrop::environment::EnvironmentSnapshot::capture();
    assert_eq!(
        snapshot.hc_endpoint().unwrap(),
        std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join("core.sock")
    );
    // SAFETY: this child runs one test and has no concurrent environment readers.
    unsafe {
        std::env::set_var("HC_LATE", "late");
    }
    let script = "import json,os; print(json.dumps(dict(os.environ)))";
    let output = namedrop::ProcessHerdr::from_snapshot(HostKind::Tui, &snapshot)
        .command()
        .args(["-c", script])
        .output()
        .unwrap();
    let env: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let leaked: Vec<_> = env
        .as_object()
        .unwrap()
        .keys()
        .filter(|key| key.starts_with("HC_"))
        .collect();
    assert!(leaked.is_empty(), "TUI child inherited HC keys: {leaked:?}");
    assert_eq!(env["HERDR_SOCKET_PATH"], "/tui.sock");
    let neutral = snapshot
        .command("/usr/bin/python3")
        .args(["-c", script])
        .output()
        .unwrap();
    let neutral: serde_json::Value = serde_json::from_slice(&neutral.stdout).unwrap();
    assert_eq!(
        neutral["HC_SOCKET"],
        snapshot.hc_endpoint().unwrap().to_str().unwrap()
    );
}

#[test]
fn tui_children_remove_synthesized_hc_socket_after_identity_resolution() {
    let snapshot = namedrop::environment::EnvironmentSnapshot::from_variables([
        ("HERDR_BIN_PATH".into(), "/usr/bin/python3".into()),
        ("XDG_RUNTIME_DIR".into(), "/private/runtime".into()),
    ]);
    assert_eq!(
        snapshot.hc_endpoint().unwrap(),
        std::path::Path::new("/private/runtime/herdr-core/default.sock")
    );
    let output = namedrop::ProcessHerdr::from_snapshot(HostKind::Tui, &snapshot)
        .command()
        .args([
            "-c",
            "import os; print(os.environ.get('HC_SOCKET', 'unset'))",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "unset\n");
}
