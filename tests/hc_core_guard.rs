use namedrop::{HerdrCommandExecutor, ProcessHerdr, runtime::HostKind};
use std::{
    env, fs,
    os::unix::{fs::PermissionsExt, net::UnixListener},
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
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
            .env("HOME", &self.0)
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
    let timeout = if expected == "deadline" {
        Duration::ZERO
    } else {
        Duration::from_millis(100)
    };
    let result = ProcessHerdr::for_host(HostKind::Hc).execute_bounded(
        &["--version"],
        timeout,
        &AtomicBool::new(expected == "cancelled"),
        1024,
    );
    match expected.as_str() {
        "ok" => assert!(result.is_ok(), "{result:?}"),
        "deadline" => assert!(result.unwrap_err().contains("timed out")),
        "cancelled" => assert!(result.unwrap_err().contains("cancelled")),
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
