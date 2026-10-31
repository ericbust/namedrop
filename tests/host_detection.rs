use std::{fs, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn cli_host_detection_table_returns_before_state_input_or_host_io() {
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
    let binary = std::env::var_os("NAMEDROP_TEST_BASE_BINARY")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_namedrop").into());
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
            .env("HOME", root.join("home"))
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
            .arg("--plugin-state")
            .arg(root.join("state"))
            .args(&case.flags)
            .args(["doctor", "--print-host", "--input-json-file"])
            .arg(root.join("missing-input"))
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let valid = match case.expected {
            Some(host) => {
                output.status.success() && stdout == format!("{host}\n") && stderr.is_empty()
            }
            None => {
                output.status.code() == Some(64)
                    && stdout.is_empty()
                    && stderr.contains("conflict")
                    && tui_flags.iter().all(|flag| stderr.contains(flag))
                    && hc_flags.iter().all(|flag| stderr.contains(flag))
            }
        };
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

