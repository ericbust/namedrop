use namedrop::{AgentRuntime, install_integration_package};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

const DAY_MS: u64 = 86_400_000;
static NEXT: AtomicU64 = AtomicU64::new(1);

struct Fixture {
    root: PathBuf,
    state: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "namedrop-notice-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let state = root.join("state");
        let home = root.join("home");
        fs::create_dir_all(&state).unwrap();
        fs::create_dir_all(&home).unwrap();
        Self { root, state, home }
    }

    fn seed(&self, version: &str) -> Value {
        let cache = json!({"schema_version":1,"checked_at":100,"last_success_at":100,"last_error":null,
            "source":{"owner":"owner","repo":"repo","requested_ref":"main","resolved_commit":"a".repeat(40),"subdir":""},
            "remote_commit":"b".repeat(40),"remote_version":version});
        self.write(&cache);
        cache
    }

    fn write(&self, cache: &Value) {
        fs::write(self.state.join("update-check.json"), cache.to_string()).unwrap();
    }

    fn notice(&self, now: u64) -> Option<Value> {
        namedrop::update_check::session_notice(
            r#"{"hook_event_name":"SessionStart","source":"startup"}"#,
            &self.state,
            Some(&self.home),
            now,
            None,
        )
    }

    fn settings(&self, value: &str) {
        let path = self
            .home
            .join(".config/herdr/plugins/config/namedrop/settings.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, value).unwrap();
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_namedrop"));
        command
            .args(["--plugin-state"])
            .arg(&self.state)
            .args(args)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("CODEX_HOME", self.home.join(".codex"))
            .env("CLAUDE_CONFIG_DIR", self.home.join(".claude"))
            .env("HERDR_BIN_PATH", self.root.join("missing-herdr"))
            .env_remove("HERDR_PLUGIN_STATE_DIR")
            .env_remove("HERDR_PANE_ID")
            .env_remove("HERDR_WORKSPACE_ID")
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("CODEX_SESSION_ID")
            .env_remove("CLAUDE_CODE_SESSION_ID");
        command
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn detached_worker_behavior() {
    let output = Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/check_update.py"
        ))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn cache_only_notice_is_global_daily_and_new_versions_rearm_it() {
    let f = Fixture::new();
    f.seed("9.10.0");
    let first = f.notice(1000).unwrap();
    assert_eq!(
        first.as_object().unwrap().keys().collect::<Vec<_>>(),
        vec!["systemMessage"]
    );
    assert_eq!(
        first["systemMessage"],
        format!(
            "Namedrop 9.10.0 is available (you have {}). Update: herdr plugin action invoke update --plugin namedrop",
            env!("CARGO_PKG_VERSION")
        )
    );
    assert_eq!(f.notice(1001), None);
    assert_eq!(f.notice(1000 + DAY_MS - 1), None);
    assert_eq!(f.notice(1000 + DAY_MS), Some(first));
    f.seed("9.11.0");
    assert!(
        f.notice(1001 + DAY_MS).unwrap()["systemMessage"]
            .as_str()
            .unwrap()
            .contains("9.11.0")
    );
}

#[test]
fn versions_compare_numerically_and_invalid_cache_never_produces_a_notice() {
    let f = Fixture::new();
    for version in [
        "0.99.99",
        env!("CARGO_PKG_VERSION"),
        "1.2.3\nmalice",
        "1.2.3\"",
        "\u{1b}[31m1.2.3",
        &"9".repeat(10_000),
        "01.2.3",
        "9.2.3-beta",
        "9.2.3+build",
        "18446744073709551616.0.0",
    ] {
        f.seed(version);
        assert_eq!(f.notice(1000), None, "{version}");
    }
    assert_eq!(f.notice(1001), None);
    fs::write(f.state.join("update-check.json"), "{").unwrap();
    assert_eq!(f.notice(1001), None);
    fs::remove_file(f.state.join("update-check.json")).unwrap();
    assert_eq!(f.notice(1001), None);
    f.seed("1.10.0");
    assert!(f.notice(1001).is_some());
}

#[test]
fn pinned_ineligible_and_opted_out_caches_are_silent() {
    let f = Fixture::new();
    let original = f.seed("9.0.0");
    for source in [
        Value::Null,
        json!({}),
        json!({"kind":"local"}),
        json!({"owner":"owner","repo":"repo","requested_ref":"a".repeat(40),"resolved_commit":"b".repeat(40),"subdir":""}),
    ] {
        let mut cache = original.clone();
        cache["source"] = source;
        f.write(&cache);
        assert_eq!(f.notice(1000), None);
    }
    f.write(&original);
    for setting in [
        "{\"check_for_updates\":false}",
        "{",
        "{\"check_for_update\":false}",
        "{\"check_for_updates\":\"false\"}",
    ] {
        f.settings(setting);
        assert_eq!(f.notice(1000), None);
    }
    f.settings("{\"check_for_updates\":true}");
    assert!(f.notice(1000).is_some());
}

#[test]
fn rebuilt_sessions_do_not_claim_the_notice_and_existing_banner_is_combined() {
    let f = Fixture::new();
    f.seed("9.0.0");
    let banner = json!({"systemMessage":"existing banner", "hookSpecificOutput":{"additionalContext":"unchanged"}});
    for source in ["clear", "compact"] {
        let input = json!({"hook_event_name":"SessionStart","source":source});
        assert_eq!(
            namedrop::update_check::session_notice(
                &input.to_string(),
                &f.state,
                Some(&f.home),
                1000,
                Some(banner.clone())
            ),
            Some(banner.clone())
        );
        assert!(!f.state.join("update-notice.json").exists());
    }
    let output = namedrop::update_check::session_notice(
        r#"{"hook_event_name":"SessionStart","source":"resume"}"#,
        &f.state,
        Some(&f.home),
        1000,
        Some(banner.clone()),
    )
    .unwrap();
    assert!(
        output["systemMessage"]
            .as_str()
            .unwrap()
            .starts_with("existing banner\nNamedrop 9.0.0")
    );
    assert_eq!(output["hookSpecificOutput"], banner["hookSpecificOutput"]);
}

#[test]
fn real_session_start_cli_reads_cache_without_herdr_or_network() {
    let f = Fixture::new();
    f.seed("9.0.0");
    let run = |event: &str| {
        let mut child = f
            .command(&[
                "hook",
                "session-start",
                "--state-directory",
                f.state.to_str().unwrap(),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(event.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    };
    let first = run(r#"{"hook_event_name":"SessionStart","source":"startup"}"#);
    assert!(first.status.success(), "{first:?}");
    let value: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert!(value["systemMessage"].as_str().unwrap().contains("9.0.0"));
    assert!(value.get("hookSpecificOutput").is_none());
    assert!(
        run(r#"{"hook_event_name":"SessionStart","source":"startup"}"#)
            .stdout
            .is_empty()
    );
}

#[test]
fn only_background_triggers_launch_the_worker_and_events_do_not_wait_for_it() {
    use std::time::{Duration, Instant};
    let f = Fixture::new();
    let bin = f.root.join("bin");
    fs::create_dir(&bin).unwrap();
    let marker = f.root.join("started");
    let release = f.root.join("release");
    let done = f.root.join("done");
    let python = bin.join("python3");
    fs::write(
        &python,
        format!(
            "#!/bin/sh\n: > '{}'\nwhile [ ! -f '{}' ]; do sleep 0.01; done\n: > '{}'\n",
            marker.display(),
            release.display(),
            done.display()
        ),
    )
    .unwrap();
    fs::set_permissions(python, fs::Permissions::from_mode(0o700)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    for args in [
        vec!["doctor"],
        vec!["address", "list"],
        vec![
            "hook",
            "session-start",
            "--state-directory",
            f.state.to_str().unwrap(),
        ],
        vec![
            "hook",
            "user-prompt-submit",
            "--state-directory",
            f.state.to_str().unwrap(),
        ],
    ] {
        f.command(&args).env("PATH", &path).output().unwrap();
        assert!(
            !marker.exists(),
            "interactive command launched update worker: {args:?}"
        );
    }
    for args in [
        ["reconcile"].as_slice(),
        ["event", "pane-lifecycle"].as_slice(),
    ] {
        fs::remove_file(&marker).ok();
        fs::remove_file(&release).ok();
        fs::remove_file(&done).ok();
        let mut cli = f
            .command(args)
            .env("PATH", &path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let exited = loop {
            if cli.try_wait().unwrap().is_some() {
                break true;
            }
            if Instant::now() >= deadline {
                break false;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        fs::write(&release, "").unwrap();
        if !exited {
            cli.kill().unwrap();
            cli.wait().unwrap();
        }
        assert!(exited, "pane handling waited for the worker");
        while !done.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            marker.exists() && done.exists(),
            "background trigger did not launch worker: {args:?}"
        );
    }
    fs::remove_file(marker.clone()).unwrap();
    f.settings("{\"check_for_updates\":false}");
    f.command(&["reconcile"])
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(!marker.exists(), "opt-out launched a worker");
}

#[test]
fn both_wrappers_forward_only_composed_notices_and_reject_hostile_versions() {
    let f = Fixture::new();
    let executable = f.root.join("hook-result");
    let update = |version: &str| {
        format!(
            "Namedrop {version} is available (you have {}). Update: herdr plugin action invoke update --plugin namedrop",
            env!("CARGO_PKG_VERSION")
        )
    };
    for runtime in [AgentRuntime::ClaudeCode, AgentRuntime::Codex] {
        let package = install_integration_package(&f.home, runtime, &executable, &f.state).unwrap();
        for (value, forward) in [
            (json!({"systemMessage":update("9.10.0")}), true),
            (
                json!({"hookSpecificOutput":{"additionalContext":"Namedrop selection policy. Existing policy.","hookEventName":"SessionStart"},"systemMessage":update("9.10.0")}),
                true,
            ),
            (
                json!({"hookSpecificOutput":{"additionalContext":"unrelated context","hookEventName":"SessionStart"},"systemMessage":update("9.10.0")}),
                false,
            ),
            (json!({"systemMessage":"unrelated message"}), false),
            (json!({"systemMessage":update("9.0.0\nmalice")}), false),
            (json!({"systemMessage":update("9.0.0\"")}), false),
            (json!({"systemMessage":update("\u{1b}[31m9.0.0")}), false),
            (json!({"systemMessage":update(&"9".repeat(10_000))}), false),
            (json!({"systemMessage":update("01.2.3")}), false),
            (json!({"systemMessage":update("9.0.0.")}), false),
            (
                json!({"systemMessage":update("9.0.0"),"unexpected":"payload"}),
                false,
            ),
        ] {
            let text = value.to_string();
            fs::write(
                &executable,
                format!(
                    "#!/bin/sh\nprintf '%s' '{}'\n",
                    text.replace('\'', "'\"'\"'")
                ),
            )
            .unwrap();
            fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
            let output = Command::new(package.join("scripts/session-start"))
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            assert_eq!(
                String::from_utf8(output.stdout).unwrap(),
                if forward { text.clone() } else { String::new() },
                "{runtime:?}: {text}"
            );
        }
    }
}
