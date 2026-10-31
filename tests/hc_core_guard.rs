#[path = "support/environment.rs"]
mod environment;
use environment::CommandHome;

#[path = "support/hc_socket.rs"]
mod hc_socket;

use namedrop::{
    HerdrCommandExecutor, ProcessHerdr,
    runtime::{HostKind, hc_socket_path, hc_state_root},
};
use std::{
    env, fs,
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = env::temp_dir().join(format!(
            "nd-hc-guard-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let fixture = Self(root);
        fixture.write("hc", "#!/bin/sh\nprintf '%s|%s|%s|%s\\n' \"$*\" \"$HC_SOCKET\" \"${HC_TERMINAL_SOCKET-unset}\" \"${HC_TERMINAL_ID-unset}\" >> \"$LOG\"\nprintf '{}'");
        fs::set_permissions(fixture.0.join("hc"), fs::Permissions::from_mode(0o700)).unwrap();
        fixture
    }
    fn write(&self, path: &str, contents: &str) {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
    fn run(&self, extra: &[(&str, &str)], expected: &str) {
        let output = Command::new(env::current_exe().unwrap())
            .args(["--exact", "guard_child", "--nocapture"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .isolated_home(&self.0)
            .env("XDG_RUNTIME_DIR", self.0.join("run"))
            .env("HC_BIN_PATH", self.0.join("hc"))
            .env("LOG", self.0.join("calls"))
            .env("GUARD_CHILD", expected)
            .envs(extra.iter().copied())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn no_calls(&self) {
        assert!(
            !self.0.join("calls").exists(),
            "stopped core launched hc: {}",
            fs::read_to_string(self.0.join("calls")).unwrap()
        );
    }
    fn listener(&self, path: &str) -> UnixListener {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        UnixListener::bind(path).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn guard_child() {
    let Ok(expected) = env::var("GUARD_CHILD") else {
        return;
    };
    if expected == "detect" {
        assert_eq!(HostKind::from_environment(None).unwrap(), HostKind::Tui);
        return;
    }
    if expected == "checked" || expected == "checked-stopped" {
        let command = ProcessHerdr::for_host(HostKind::Hc).checked_command();
        if expected == "checked-stopped" {
            assert!(command.is_err());
        } else {
            assert!(
                command
                    .unwrap()
                    .arg("--version")
                    .status()
                    .unwrap()
                    .success()
            );
        }
        return;
    }
    let timeout = if expected == "deadline" {
        Duration::ZERO
    } else {
        Duration::from_millis(100)
    };
    let cancelled = AtomicBool::new(expected == "cancelled");
    let start = Instant::now();
    let result = std::thread::scope(|scope| {
        if expected == "waiting-cancelled" {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(20));
                cancelled.store(true, Ordering::Relaxed);
            });
        }
        ProcessHerdr::for_host(HostKind::Hc).execute_bounded(
            &["--version"],
            timeout,
            &cancelled,
            1024,
        )
    });
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "preflight exceeded budget"
    );
    match expected.as_str() {
        "ok" => assert!(result.is_ok(), "{result:?}"),
        "deadline" | "waiting-deadline" => assert!(result.unwrap_err().contains("timed out")),
        "cancelled" | "waiting-cancelled" => assert!(result.unwrap_err().contains("cancelled")),
        _ => assert!(
            result.is_err(),
            "hc must not launch without a connectable selected socket: {result:?}"
        ),
    }
}

#[test]
fn stopped_core_never_launches_even_version() {
    let f = Fixture::new();
    f.run(&[], "stopped");
    f.run(&[], "checked-stopped");
    f.no_calls();
}

#[test]
fn plain_file_and_stale_socket_never_launch() {
    for stale in [false, true] {
        let f = Fixture::new();
        if stale {
            drop(f.listener("endpoint"));
        } else {
            f.write("endpoint", "not a socket");
        }
        f.run(
            &[("HC_SOCKET", f.0.join("endpoint").to_str().unwrap())],
            "stopped",
        );
        f.no_calls();
    }
}

#[test]
fn doctor_explains_overlong_socket_paths_without_launching_hc() {
    // SAFETY: an all-zero sockaddr_un is valid for inspecting its pathname capacity.
    let address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let max_bytes = address.sun_path.len() - 1;
    for source in [
        "HC_SOCKET",
        "HC_TERMINAL_SOCKET",
        "XDG_RUNTIME_DIR",
        "config",
    ] {
        for character in ["x", "é"] {
            let f = Fixture::new();
            let long_path = format!("/{}", character.repeat(max_bytes.div_ceil(character.len())));
            let socket = if source == "XDG_RUNTIME_DIR" {
                format!("{long_path}/herdr-core/default.sock")
            } else {
                long_path.clone()
            };
            let mut command = Command::new(env!("CARGO_BIN_EXE_namedrop"));
            command
                .args(["--host", "hc", "--plugin-state"])
                .arg(f.0.join("state"))
                .args(["doctor", "--json"])
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .isolated_home(&f.0)
                .env("XDG_RUNTIME_DIR", f.0.join("run"))
                .env("HC_BIN_PATH", f.0.join("hc"))
                .env("LOG", f.0.join("calls"));
            if source == "config" {
                f.write(
                    ".config/herdr-core/config.toml",
                    &format!("socket = {long_path:?}"),
                );
            } else {
                command.env(source, &long_path);
            }
            let output = command.output().unwrap();
            f.no_calls();
            assert_eq!(output.status.code(), Some(2), "{output:?}");
            let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            let message = report["error"]["message"].as_str().unwrap();
            for expected in [
                "hc socket path is too long".to_owned(),
                format!("{} bytes", socket.len()),
                format!("maximum {max_bytes} bytes"),
                "HC_SOCKET".to_owned(),
                "shorter path".to_owned(),
                socket,
            ] {
                assert!(
                    message.contains(&expected),
                    "{source} doctor message lacks {expected:?}: {message}"
                );
            }
        }
    }
}

#[test]
fn socket_path_at_platform_byte_limit_can_connect() {
    // SAFETY: an all-zero sockaddr_un is valid for inspecting its pathname capacity.
    let address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let max_bytes = address.sun_path.len() - 1;
    for character in ["x", "é"] {
        let f = Fixture::new();
        let remaining = max_bytes - f.0.as_os_str().len() - 1;
        let filename = format!(
            "{}{}",
            character.repeat(remaining / character.len()),
            "x".repeat(remaining % character.len())
        );
        let endpoint = f.0.join(&filename);
        assert_eq!(endpoint.as_os_str().len(), max_bytes);
        let _listener = f.listener(&filename);
        f.run(&[("HC_SOCKET", endpoint.to_str().unwrap())], "ok");
        assert!(f.0.join("calls").exists());
    }
}

#[test]
fn socket_precedence_and_child_routing() {
    for selected in ["explicit", "inherited", "config", "runtime"] {
        let f = Fixture::new();
        let path = if selected == "runtime" {
            "run/herdr-core/default.sock"
        } else {
            selected
        };
        let listener = f.listener(path);
        let endpoint = f.0.join(path);
        f.write(
            ".config/herdr-core/config.toml",
            &format!("socket = {:?}\n", f.0.join("config")),
        );
        let mut extra = vec![("HC_TERMINAL_ID", "t42")];
        let explicit = f.0.join("explicit");
        let inherited = f.0.join("inherited");
        if selected == "explicit" {
            extra.push(("HC_SOCKET", explicit.to_str().unwrap()));
        }
        if matches!(selected, "explicit" | "inherited") {
            extra.push(("HC_TERMINAL_SOCKET", inherited.to_str().unwrap()));
        }
        if selected == "runtime" {
            f.write(".config/herdr-core/config.toml", "");
        }
        f.run(&extra, "ok");
        assert_eq!(
            fs::read_to_string(f.0.join("calls")).unwrap(),
            format!("--version|{}|unset|unset\n", endpoint.display())
        );
        drop(listener);
    }
}

#[test]
fn invalid_session_or_config_never_launches() {
    for config in ["socket = 42", "socket = ["] {
        let f = Fixture::new();
        let _listener = f.listener("run/herdr-core/default.sock");
        f.write(".config/herdr-core/config.toml", config);
        f.run(&[], "stopped");
        f.no_calls();
    }
    let f = Fixture::new();
    let _listener = f.listener("endpoint");
    f.run(
        &[
            ("HC_SESSION", "../bad"),
            ("HC_SOCKET", f.0.join("endpoint").to_str().unwrap()),
        ],
        "stopped",
    );
    f.no_calls();
}

#[test]
fn deadline_and_cancel_never_launch() {
    for expected in ["deadline", "cancelled"] {
        let f = Fixture::new();
        let _listener = f.listener("run/herdr-core/default.sock");
        f.run(&[], expected);
        f.no_calls();
    }
}

#[test]
fn inherited_identity_detector_cannot_start_stopped_core() {
    let f = Fixture::new();
    f.write(
        "herdr",
        "#!/bin/sh\nprintf '{\"result\":{\"process_info\":{\"shell_pid\":1}}}'",
    );
    fs::set_permissions(f.0.join("herdr"), fs::Permissions::from_mode(0o700)).unwrap();
    f.run(
        &[
            ("HERDR_BIN_PATH", f.0.join("herdr").to_str().unwrap()),
            ("HERDR_PANE_ID", "p1"),
            ("HC_TERMINAL_ID", "t1"),
        ],
        "detect",
    );
    f.no_calls();
}

#[test]
fn checked_commands_reuse_an_accepting_listener_and_scrub_identity() {
    let f = Fixture::new();
    let endpoint = f.0.join("endpoint");
    let _listener = hc_socket::HcSocket::bind(&endpoint).unwrap();
    for _ in 0..4 {
        f.run(&[("HC_SOCKET", endpoint.to_str().unwrap())], "checked");
    }
    assert_eq!(
        fs::read_to_string(f.0.join("calls"))
            .unwrap()
            .lines()
            .count(),
        4
    );
}

#[test]
fn missing_explicit_socket_does_not_fall_back_to_a_running_core() {
    let f = Fixture::new();
    let _listener = f.listener("run/herdr-core/default.sock");
    f.run(
        &[("HC_SOCKET", f.0.join("absent").to_str().unwrap())],
        "stopped",
    );
    f.no_calls();
}

#[test]
fn xdg_config_overrides_home_and_empty_socket_variables_are_ignored() {
    let f = Fixture::new();
    let _listener = f.listener("endpoint");
    f.write(".config/herdr-core/config.toml", "socket = '/absent'");
    f.write(
        "config/herdr-core/config.toml",
        &format!("socket = {:?}", f.0.join("endpoint")),
    );
    f.run(
        &[
            ("XDG_CONFIG_HOME", f.0.join("config").to_str().unwrap()),
            ("HC_SOCKET", ""),
            ("HC_TERMINAL_SOCKET", ""),
        ],
        "ok",
    );
}

#[test]
fn state_root_and_session_paths_follow_xdg_precedence() {
    let variables = |key: &str| match key {
        "XDG_STATE_HOME" => Some("/state".to_owned()),
        "HOME" => Some("/home/fixture".to_owned()),
        "HC_SESSION" => Some(" chosen ".to_owned()),
        _ => None,
    };
    assert_eq!(hc_state_root(variables).unwrap(), PathBuf::from("/state"));
    assert_eq!(
        hc_state_root(|key| if key == "XDG_STATE_HOME" {
            Some("".into())
        } else {
            variables(key)
        })
        .unwrap(),
        PathBuf::from("/home/fixture/.local/state")
    );
    assert!(hc_state_root(|_| None).is_err());
    // SAFETY: getuid has no arguments or memory preconditions.
    let runtime = PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }));
    let expected = if runtime.is_dir() {
        runtime.join("herdr-core/chosen.sock")
    } else {
        PathBuf::from("/state/herdr-core/chosen/chosen.sock")
    };
    assert_eq!(hc_socket_path(variables).unwrap(), expected);
}

#[cfg(target_os = "linux")]
#[test]
fn full_accept_queue_obeys_deadline_and_cancellation() {
    use std::os::{fd::AsRawFd, unix::net::UnixStream};
    for expected in ["waiting-deadline", "waiting-cancelled"] {
        let f = Fixture::new();
        let listener = f.listener("run/herdr-core/default.sock");
        // SAFETY: the listener owns this descriptor; backlog zero permits one queued connection on Linux.
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 0) }, 0);
        let _queued = UnixStream::connect(f.0.join("run/herdr-core/default.sock")).unwrap();
        f.run(&[], expected);
        f.no_calls();
    }
}
