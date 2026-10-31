#[path = "support/environment.rs"]
mod environment;
use environment::CommandHome;
use namedrop::{cli::RuntimeOptions, runtime::HostKind};
use std::{fs, process::Command};

#[test]
fn runtime_roots_stay_at_the_construction_environment() {
    if std::env::var_os("NAMEDROP_SNAPSHOT_CHILD").is_none() {
        let root = std::env::temp_dir().join(format!("namedrop-snapshot-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
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
