#[path = "support/environment.rs"]
mod environment;
use environment::CommandHome;

#[path = "support/hc_socket.rs"]
mod hc_socket;
use std::{fs, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn snapshot_host_detection_table_returns_before_state_or_dispatch() {
    struct Case {
        name: String,
        flags: Vec<&'static str>,
        env: Vec<&'static str>,
        expected: Option<&'static str>,
        ancestry: Option<(&'static str, &'static str)>,
        expected_probes: usize,
    }
    let mut cases = Vec::new();
    let mut add = |name: &str, flags, env, expected| {
        cases.push(Case {
            name: name.into(),
            flags,
            env,
            expected,
            ancestry: None,
            expected_probes: 0,
        });
    };
    add("plain shell defaults to tui", vec![], vec![], Some("tui"));
    add(
        "irrelevant HC variables do not select hc",
        vec![],
        vec![
            "HC_SESSION",
            "HC_SOCKET",
            "HC_TERMINAL_SOCKET",
            "HC_SPACE_ID",
        ],
        Some("tui"),
    );
    add(
        "hc terminal environment",
        vec![],
        vec!["HC_TERMINAL_ID"],
        Some("hc"),
    );
    for key in ["HERDR_SOCKET_PATH", "HERDR_PANE_ID"] {
        add(key, vec![], vec![key], Some("tui"));
        add(
            &format!("{key} inside hc"),
            vec![],
            vec![key, "HC_TERMINAL_ID"],
            Some("tui"),
        );
    }
    let tui_flags = ["--herdr-socket", "--herdr-workspace", "--herdr-pane"];
    let hc_flags = ["--hc-terminal", "--hc-space"];
    for flag in tui_flags {
        add(
            flag,
            vec![flag, "value"],
            vec!["HC_TERMINAL_ID"],
            Some("tui"),
        );
    }
    for flag in hc_flags {
        add(
            flag,
            vec![flag, "value"],
            vec!["HERDR_SOCKET_PATH", "HERDR_PANE_ID"],
            Some("hc"),
        );
    }
    for tui in tui_flags {
        for hc in hc_flags {
            for flags in [
                vec![tui, "value", hc, "value"],
                vec![hc, "value", tui, "value"],
            ] {
                add(&format!("conflict {flags:?}"), flags.clone(), vec![], None);
                for host in ["tui", "hc"] {
                    let mut explicit = flags.clone();
                    explicit.extend(["--host", host]);
                    add(
                        &format!("override {explicit:?}"),
                        explicit,
                        vec!["HERDR_PANE_ID", "HC_TERMINAL_ID"],
                        Some(host),
                    );
                }
            }
        }
    }
    add(
        "explicit hc overrides tui environment",
        vec!["--host", "hc"],
        vec!["HERDR_SOCKET_PATH"],
        Some("hc"),
    );
    add(
        "explicit tui overrides hc environment",
        vec!["--host", "tui"],
        vec!["HC_TERMINAL_ID"],
        Some("tui"),
    );

    for (name, flags, expected) in [
        (
            "plugin state selects tui",
            vec!["--plugin-state", "/explicit-state"],
            Some("tui"),
        ),
        (
            "plugin state conflicts with hc transport",
            vec![
                "--plugin-state",
                "/explicit-state",
                "--hc-terminal",
                "terminal",
            ],
            None,
        ),
        (
            "explicit hc accepts plugin state",
            vec!["--host", "hc", "--plugin-state", "/explicit-state"],
            Some("hc"),
        ),
        (
            "explicit tui overrides hc transport",
            vec!["--host", "tui", "--hc-space", "space"],
            Some("tui"),
        ),
        (
            "explicit hc overrides tui transport",
            vec!["--host", "hc", "--herdr-pane", "pane"],
            Some("hc"),
        ),
    ] {
        add(
            name,
            flags,
            vec!["HC_TERMINAL_ID", "HERDR_PANE_ID"],
            expected,
        );
    }

    for (name, tui_pid, hc_pid, expected, expected_probes) in [
        ("hc run from TUI selects nearer HC", "$far", "$$", "hc", 2),
        (
            "nested TUI in app selects nearer TUI",
            "$$",
            "$far",
            "tui",
            2,
        ),
        ("stale TUI ancestor selects HC", "999999999", "$$", "hc", 2),
        (
            "unrelated HC keeps ancestor TUI",
            "$$",
            "999999999",
            "tui",
            2,
        ),
        ("same ancestor tie selects TUI", "$$", "$$", "tui", 2),
        (
            "neither PID is ancestor selects HC",
            "999999998",
            "999999999",
            "hc",
            2,
        ),
        ("zero TUI PID falls back to TUI", "0", "$$", "tui", 1),
        (
            "overflow TUI PID falls back to TUI",
            "4294967296",
            "$$",
            "tui",
            1,
        ),
        ("zero HC PID falls back to TUI", "$$", "0", "tui", 2),
        (
            "overflow HC PID falls back to TUI",
            "$$",
            "4294967296",
            "tui",
            2,
        ),
        ("malformed HC PID falls back to TUI", "$$", "null", "tui", 2),
    ] {
        cases.push(Case {
            name: name.into(),
            flags: vec![],
            env: vec!["HERDR_SOCKET_PATH", "HERDR_PANE_ID", "HC_TERMINAL_ID"],
            expected: Some(expected),
            ancestry: Some((tui_pid, hc_pid)),
            expected_probes,
        });
    }

    let root = std::env::temp_dir().join(format!("namedrop-host-detection-{}", std::process::id()));
    fs::create_dir_all(root.join("state/runtime")).unwrap();
    fs::create_dir_all(root.join("home")).unwrap();
    let socket = root.join("hc.sock");
    let _socket = hc_socket::HcSocket::bind(&socket).unwrap();
    let probe = root.join("probe");
    fs::write(
        &probe,
        "#!/bin/sh\nprintf touched > \"$HOME/touched\"\nexit 91\n",
    )
    .unwrap();
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o700)).unwrap();
    fs::copy(&probe, root.join("state/runtime/namedrop")).unwrap();
    let identity = root.join("identity");
    fs::write(
        &identity,
        r#"#!/bin/sh
printf '%s\n' "$*" >> "$IDENTITY_LOG"
case "$1" in
  pane) printf '{"result":{"process_info":{"shell_pid":%s}}}\n' "$TUI_PID" ;;
  get) printf '{"pid":%s}\n' "$HC_PID" ;;
  *) exit 92 ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&identity, fs::Permissions::from_mode(0o700)).unwrap();
    let binary = std::env::current_exe().unwrap();
    let mut failures = Vec::new();
    for case in cases {
        let mut command = Command::new("/bin/sh");
        let script = if let Some((tui, hc)) = case.ancestry {
            format!("export TUI_PID={tui} HC_PID={hc}; \"$@\"; status=$?; exit \"$status\"")
        } else {
            "exec \"$@\"".into()
        };
        let output = command
            .args(["-c", &script, "host-detection"])
            .arg(&binary)
            .env_clear()
            .isolated_home(root.join("home"))
            .env("PATH", "/usr/bin:/bin")
            .env(
                "HC_BIN_PATH",
                if case.ancestry.is_some() {
                    &identity
                } else {
                    &probe
                },
            )
            .env(
                "HERDR_BIN_PATH",
                if case.ancestry.is_some() {
                    &identity
                } else {
                    &probe
                },
            )
            .env("far", std::process::id().to_string())
            .env("IDENTITY_LOG", root.join("identity-log"))
            .envs(case.env.iter().map(|key| (key, "value")))
            .env("HC_SOCKET", &socket)
            .env("HERDR_PLUGIN_STATE_DIR", root.join("state"))
            .env("NAMEDROP_STATE_DIR", root.join("state"))
            .env("HOST_FLAGS", serde_json::to_string(&case.flags).unwrap())
            .env("EXPECTED_HOST", case.expected.unwrap_or("conflict"))
            .args([
                "--ignored",
                "--exact",
                "snapshot_host_detection_child",
                "--nocapture",
            ])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let valid = output.status.success();
        println!(
            "{} => status={} stdout={stdout:?} stderr={stderr:?}",
            case.name, output.status
        );
        if !valid {
            failures.push(case.name.clone());
        }
        let ambiguous = case.flags.is_empty()
            && case.env.contains(&"HC_TERMINAL_ID")
            && case.env.contains(&"HERDR_PANE_ID");
        if ambiguous {
            let _ = fs::remove_file(root.join("home/touched"));
        } else {
            assert!(
                !root.join("home/touched").exists(),
                "host or runtime executed"
            );
        }
        if case.ancestry.is_some() {
            let probes = fs::read_to_string(root.join("identity-log")).unwrap_or_default();
            if probes.lines().count() != case.expected_probes {
                failures.push(format!(
                    "{} expected {} identity lookups, observed {probes:?}",
                    case.name, case.expected_probes
                ));
            }
            let _ = fs::remove_file(root.join("identity-log"));
        }
        assert!(
            !root.join("state/runtime/commands.lock").exists(),
            "runtime gate accessed"
        );
        assert_eq!(
            fs::read_dir(root.join("state")).unwrap().count(),
            1,
            "state opened"
        );
        assert_eq!(
            fs::read_dir(root.join("home")).unwrap().count(),
            0,
            "home changed"
        );
    }
    fs::remove_dir_all(root).unwrap();
    assert!(
        failures.is_empty(),
        "incorrect host detection rows: {failures:?}"
    );
}

#[test]
fn ambiguous_host_identity_is_cached_and_bounded_across_both_native_lookups() {
    let root = std::env::temp_dir().join(format!("namedrop-host-budget-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let socket = root.join("hc.sock");
    let _socket = hc_socket::HcSocket::bind(&socket).unwrap();
    let probe = root.join("identity");
    fs::write(&probe, r#"#!/usr/bin/python3
import json, os, sys, time
host = 'tui' if sys.argv[1] == 'pane' else 'hc'
with open(os.environ['IDENTITY_LOG'], 'a') as log:
    log.write(host + '\n')
mode = os.environ['IDENTITY_MODE']
if mode == 'deadline':
    time.sleep(0.95)
if mode == host + '-failure':
    sys.exit(91)
if mode == 'oversized':
    print(' ' * (512 * 1024 + 1))
else:
    print(json.dumps({'result': {'process_info': {'shell_pid': 999999998}}} if host == 'tui' else {'pid': 999999999}))
"#).unwrap();
    fs::set_permissions(&probe, fs::Permissions::from_mode(0o700)).unwrap();
    for (mode, expected, probes) in [
        ("cache", "hc", 2),
        ("tui-failure", "tui", 1),
        ("hc-failure", "tui", 2),
        ("oversized", "tui", 1),
        ("deadline", "tui", 2),
    ] {
        let log = root.join(mode);
        let start = std::time::Instant::now();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "cached_host_child", "--nocapture"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HERDR_PANE_ID", "tui")
            .env("HC_TERMINAL_ID", "hc")
            .env("HERDR_BIN_PATH", &probe)
            .env("HC_BIN_PATH", &probe)
            .env("HC_SOCKET", &socket)
            .env("IDENTITY_LOG", &log)
            .env("IDENTITY_MODE", mode)
            .env("EXPECTED_HOST", expected)
            .env("HERDR_PLUGIN_STATE_DIR", "/unused-state")
            .env("NAMEDROP_STATE_DIR", "/unused-state")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{mode}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            start.elapsed() < std::time::Duration::from_secs(3),
            "{mode} exceeded the shared identity deadline"
        );
        assert_eq!(
            fs::read_to_string(log).unwrap().lines().count(),
            probes,
            "{mode}"
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore]
fn cached_host_child() {
    use namedrop::{cli::RuntimeOptions, runtime::HostKind};
    let options = RuntimeOptions {
        ..RuntimeOptions::default()
    };
    let expected = if std::env::var("EXPECTED_HOST").unwrap() == "hc" {
        HostKind::Hc
    } else {
        HostKind::Tui
    };
    assert_eq!(options.ambient_host_kind(), expected);
    assert_eq!(options.host_kind().unwrap(), expected);
    assert_eq!(options.clone().host_kind().unwrap(), expected);
    assert_eq!(
        options.state_directory().unwrap(),
        std::path::Path::new("/unused-state")
    );
    let explicit = RuntimeOptions {
        host: Some(HostKind::Tui),
        ..options.clone()
    };
    assert_eq!(explicit.host_kind().unwrap(), HostKind::Tui);
}

#[test]
fn describe_validates_conflicts_before_runtime_dispatch_and_keeps_explicit_state() {
    let root = std::env::temp_dir().join(format!(
        "namedrop-describe-selection-{}",
        std::process::id()
    ));
    fs::create_dir_all(root.join("state/runtime")).unwrap();
    let tripwire = root.join("tripwire");
    let marker = root.join("contacted");
    fs::write(
        &tripwire,
        format!(
            "#!/bin/sh\nprintf touched > '{}'\nexit 97\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&tripwire, fs::Permissions::from_mode(0o700)).unwrap();
    fs::copy(&tripwire, root.join("state/runtime/namedrop")).unwrap();
    for flags in [
        vec!["--hc-terminal", "terminal", "--herdr-pane", "pane"],
        vec![],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_namedrop"))
            .env_clear()
            .env("HC_TERMINAL_ID", "terminal")
            .env("HERDR_PANE_ID", "pane")
            .env("HC_BIN_PATH", &tripwire)
            .env("HERDR_BIN_PATH", &tripwire)
            .arg("--plugin-state")
            .arg(root.join("state"))
            .args(&flags)
            .args(["--json", "describe"])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(if flags.is_empty() { 97 } else { 64 })
        );
        if !flags.is_empty() {
            let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert!(body.to_string().contains("invalid_request"));
            assert!(
                !marker.exists(),
                "conflicting describe accessed runtime or host"
            );
            assert!(!root.join("state/runtime/commands.lock").exists());
        } else {
            assert!(marker.exists(), "describe bypassed selected runtime");
        }
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore]
fn snapshot_host_detection_child() {
    use clap::Parser;
    let flags: Vec<String> = serde_json::from_str(&std::env::var("HOST_FLAGS").unwrap()).unwrap();
    let cli = namedrop::cli::Cli::try_parse_from(
        std::iter::once("namedrop".to_owned())
            .chain(flags.clone())
            .chain(["describe".to_owned()]),
    )
    .unwrap();
    let expected = std::env::var("EXPECTED_HOST").unwrap();
    if expected == "conflict" {
        let error = cli.runtime.host_kind().unwrap_err();
        for flag in [
            "--herdr-socket",
            "--herdr-workspace",
            "--herdr-pane",
            "--hc-terminal",
            "--hc-space",
            "--plugin-state",
        ] {
            assert_eq!(
                error.contains(flag),
                flags.iter().any(|arg| arg == flag),
                "conflict should name only supplied flags: {error}"
            );
        }
    } else {
        let expected = if expected == "hc" {
            namedrop::runtime::HostKind::Hc
        } else {
            namedrop::runtime::HostKind::Tui
        };
        assert_eq!(cli.runtime.host_kind().unwrap(), expected);
    }
}
