#[path = "support/environment.rs"]
mod environment;
#[path = "support/hc_socket.rs"]
mod hc_socket;

use environment::CommandHome;
use namedrop::{HcHost, HerdrCommandExecutor, HerdrCommandOutput, Host, Popup, ProcessHerdr};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

const CHILD: &str = "NAMEDROP_POPUP_SNAPSHOT_CHILD";
const ROOTS: &[(&str, &str)] = &[
    ("HOME", ""),
    ("XDG_STATE_HOME", ".local/state"),
    ("XDG_CONFIG_HOME", ".config"),
    ("XDG_DATA_HOME", ".local/share"),
    ("XDG_RUNTIME_DIR", ".runtime"),
    ("PATH", "bin"),
    ("HC_SOCKET", "core.sock"),
];

#[test]
fn hc_popup_retains_executor_snapshot() {
    if std::env::var_os(CHILD).is_none() {
        for (entrypoint, program) in [
            ("tour", "absolute"),
            ("doctor", "absolute"),
            ("agent-status", "path"),
            ("naming", "absolute"),
        ] {
            run_child("hc_popup_retains_executor_snapshot", entrypoint, program);
        }
        return;
    }
    let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    let options = namedrop::cli::RuntimeOptions::default();
    let executor =
        ProcessHerdr::from_snapshot(namedrop::runtime::HostKind::Hc, &options.environment);
    change_environment(&home);
    assert_popup(HcHost::new(&executor), &home, "A");
}

#[test]
fn hc_popup_without_executor_support_is_unavailable() {
    if std::env::var_os(CHILD).is_none() {
        run_child(
            "hc_popup_without_executor_support_is_unavailable",
            "doctor",
            "absolute",
        );
        return;
    }
    let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
    let executor = NoSnapshot;
    let host = HcHost::new(&executor);
    change_environment(&home);
    let popup = Popup {
        entrypoint: "doctor",
        width: "72",
        height: "24",
        env: &[],
    };
    for use_override in [true, false] {
        if !use_override {
            // This child runs one test without concurrent environment readers.
            unsafe { std::env::remove_var("HERDR_APP_BIN_PATH") };
        }
        let error = host.open_popup(&popup).unwrap_err();
        assert_eq!(
            error.message,
            "HC popups are unavailable with this executor"
        );
    }
}

struct NoSnapshot;

impl HerdrCommandExecutor for NoSnapshot {
    fn execute(&self, _: &[&str]) -> Result<HerdrCommandOutput, String> {
        panic!("opening an app popup must not invoke hc");
    }
}

fn run_child(test: &str, entrypoint: &str, program: &str) {
    let root = hc_socket::fixture::short_socket_root();
    for label in ["A", "B"] {
        let bin = root.join(label).join("bin");
        fs::create_dir_all(&bin).unwrap();
        let app = bin.join("snapshot-app");
        fs::write(
            &app,
            format!(
                r#"#!/usr/bin/python3
import json, os, sys
with open(os.path.join(os.path.dirname(__file__), 'app-calls'), 'a') as log:
    log.write('called\n')
keys = ["HOME", "XDG_STATE_HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "XDG_RUNTIME_DIR", "PATH", "HC_SOCKET", "HC_SESSION"]
print(json.dumps({{"program": "{label}", "argv": sys.argv[1:], "env": {{k: v for k, v in os.environ.items() if k in keys or k.startswith("HERDR_") or k.startswith("HC_TERMINAL_")}}}}))
"#
            ),
        )
        .unwrap();
        fs::set_permissions(&app, fs::Permissions::from_mode(0o755)).unwrap();
        let tripwire = bin.join("herdr-app");
        fs::write(
            &tripwire,
            r#"#!/usr/bin/python3
import os, sys
with open(os.path.join(os.path.dirname(__file__), 'unexpected-app'), 'w') as log:
    log.write('called')
sys.exit(97)
"#,
        )
        .unwrap();
        fs::set_permissions(&tripwire, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let home = root.join("A");
    let _cores = [
        hc_socket::HcSocket::bind(&home.join("core.sock")).unwrap(),
        hc_socket::HcSocket::bind(&root.join("B/core.sock")).unwrap(),
    ];
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", test, "--nocapture"])
        .isolated_env(&home)
        .env("PATH", home.join("bin"))
        .env("HC_SOCKET", home.join("core.sock"))
        .env("HC_SESSION", "A")
        .env("HC_BIN_PATH", "/must-not-run-hc")
        .env("HERDR_CAPTURED_ONLY", "captured")
        .env("HC_TERMINAL_ID", "captured-terminal")
        .env("HC_TERMINAL_SOCKET", "captured-terminal.sock")
        .env(CHILD, entrypoint);
    match program {
        "absolute" => {
            command.env("HERDR_APP_BIN_PATH", home.join("bin/snapshot-app"));
        }
        "path" => {
            command.env("HERDR_APP_BIN_PATH", "snapshot-app");
        }
        _ => unreachable!(),
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{entrypoint}/{program}\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    for label in ["A", "B"] {
        let bin = root.join(label).join("bin");
        assert!(!bin.join("unexpected-app").exists());
        let calls = fs::read_to_string(bin.join("app-calls")).unwrap_or_default();
        let expected = if test == "hc_popup_retains_executor_snapshot" && label == "A" {
            "called\n"
        } else {
            ""
        };
        assert_eq!(calls, expected, "{test}/{label}");
    }
    fs::remove_dir_all(root).unwrap();
}

fn change_environment(home: &Path) {
    let late = home.parent().unwrap().join("B");
    // The subprocess runs exactly one test and starts no concurrent environment readers.
    unsafe {
        for (key, suffix) in ROOTS {
            std::env::set_var(key, root_path(&late, suffix));
        }
        std::env::set_var("HC_SESSION", "B");
        std::env::set_var("HERDR_APP_BIN_PATH", late.join("bin/snapshot-app"));
        std::env::remove_var("HERDR_CAPTURED_ONLY");
        std::env::set_var("HERDR_LATE_ONLY", "late");
        std::env::set_var("HC_TERMINAL_ID", "late-terminal");
        std::env::set_var("HC_TERMINAL_SOCKET", "late-terminal.sock");
    }
}

fn assert_popup(host: impl Host, home: &Path, program: &str) {
    let entrypoint = std::env::var(CHILD).unwrap();
    let mod_name = if entrypoint == "naming" {
        "namedrop-name".to_owned()
    } else {
        format!("namedrop-{entrypoint}")
    };
    let actual = host
        .open_popup(&Popup {
            entrypoint: &entrypoint,
            width: "72",
            height: "24",
            env: &[],
        })
        .unwrap();
    let mut expected_env = serde_json::Map::new();
    for (key, suffix) in ROOTS {
        expected_env.insert(
            (*key).to_owned(),
            root_path(home, suffix)
                .into_os_string()
                .into_string()
                .unwrap()
                .into(),
        );
    }
    expected_env.insert("HC_SESSION".into(), program.into());
    assert_eq!(
        actual,
        serde_json::json!({
            "program": program,
            "argv": ["do", "mod.open", "--name", mod_name],
            "env": expected_env,
        })
    );
}

fn root_path(home: &Path, suffix: &str) -> std::path::PathBuf {
    if suffix.is_empty() {
        home.to_owned()
    } else {
        home.join(suffix)
    }
}
