//! Which Herdr server a command talks to. Every `herdr` the CLI starts
//! inherits `HERDR_SOCKET_PATH` unless `--herdr-socket` names one, and the
//! session's socket is the flag's, the environment's, or the one `status
//! server` reports, in that order. The mailbox wake hook takes no flag, so
//! its `herdr` inherits the variable too.
//!
//! Each location also has a neutral name (`namedrop::locator`): a flag beside
//! each `--herdr-*` one, and a `NAMEDROP_*` variable that counts only where
//! none of `HERDR_SOCKET_PATH`, `HERDR_WORKSPACE_ID`, and `HERDR_PANE_ID` is
//! set.

#[path = "support/executable.rs"]
mod executable;

use std::{
    fs,
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

trait Finish {
    /// Runs the command to its end, or fails once it has run 20 s: a command
    /// that retries a space it can never find would otherwise hang the test.
    fn finish(&mut self) -> Output;
}

impl Finish for Command {
    fn finish(&mut self) -> Output {
        let mut child = self
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() > deadline {
                let _ = child.kill();
                panic!("still running after 20 s: {self:?}");
            }
            thread::sleep(Duration::from_millis(20));
        }
        child.wait_with_output().unwrap()
    }
}

/// Logs each command with the socket it was given, and answers one pane, a
/// popup, `status server` with the socket in `$ROOT/served`, or fails it
/// when that file is missing, and workspace w1 with a checkout at
/// `$CHECKOUT`, or `/repo`.
const HERDR: &str = r#"#!/bin/sh
printf 'socket<%s> %s\n' "${HERDR_SOCKET_PATH-unset}" "$*" >> "$ROOT/calls"
case "$1 $2" in
"status server")
  [ -f "$ROOT/served" ] || { echo 'no server is running' >&2; exit 1; }
  printf 'running: yes\nsocket: %s\n' "$(cat "$ROOT/served")" ;;
"worktree list")
  printf '{"result":{"worktrees":[{"path":"%s","open_workspace_id":"w1"}]}}\n' "${CHECKOUT:-/repo}" ;;
"workspace list")
  printf '%s\n' '{"result":{"workspaces":[{"workspace_id":"w1"}]}}' ;;
"pane list")
  printf '%s\n' '{"result":{"panes":[{"pane_id":"p1","terminal_id":"t1","tab_id":"tab1","workspace_id":"w1","agent":"codex","agent_session":{"value":"session-1"},"agent_status":"idle"}]}}' ;;
"plugin pane")
  printf '%s\n' '{"result":{"pane_id":"popup"}}' ;;
*) printf '%s\n' '{}' ;;
esac
"#;

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "namedrop-herdr-socket-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        executable::write_executable(root.join("herdr"), HERDR);
        Self(root)
    }

    /// `namedrop` with this fixture's `herdr`, state, and home, and no Herdr
    /// pane, workspace, or socket of its own.
    fn namedrop(&self, inherited: Option<&str>) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_namedrop"));
        command
            .env("ROOT", &self.0)
            .env("TMPDIR", &self.0)
            .env("HERDR_BIN_PATH", self.0.join("herdr"))
            .env("HERDR_PLUGIN_STATE_DIR", self.0.join("state"))
            .env("HOME", self.0.join("home"))
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_PANE_ID")
            .env_remove("HERDR_WORKSPACE_ID")
            .env_remove("NAMEDROP_STATE_DIR")
            .env_remove("NAMEDROP_ENDPOINT")
            .env_remove("NAMEDROP_SPACE_ID")
            .env_remove("NAMEDROP_PANE_ID")
            .env_remove("CODEX_SESSION_ID")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("NAMEDROP_HERDR_TIMEOUT_MS");
        if let Some(socket) = inherited {
            command.env("HERDR_SOCKET_PATH", socket);
        }
        command
    }

    /// Opens the Doctor popup from pane p1, so the session's socket shows in
    /// the popup's `HERDR_SOCKET_PATH`.
    fn launch(&self, flag: Option<&str>, inherited: Option<&str>) -> Vec<String> {
        let mut command = self.namedrop(inherited);
        if let Some(socket) = flag {
            command.args(["--herdr-socket", socket]);
        }
        let output = command
            .args(["--herdr-workspace", "w1", "--herdr-pane", "p1"])
            .args(["ui", "launch", "doctor"])
            .finish();
        assert!(output.status.success(), "{output:?}");
        self.calls()
    }

    /// Runs `ui launch doctor` with `flags`, and every `herdr` command it ran.
    fn launch_doctor(
        &self,
        mut command: Command,
        flags: &[&str],
    ) -> (std::process::Output, Vec<String>) {
        let output = command
            .args(flags)
            .args(["ui", "launch", "doctor"])
            .finish();
        (output, self.calls())
    }

    fn calls(&self) -> Vec<String> {
        fs::read_to_string(self.0.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn serve(&self, socket: &str) {
        fs::write(self.0.join("served"), socket).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Every command ran with `socket`, in this order, and the popup opened on
/// `session`.
fn assert_ran(calls: &[String], socket: &str, status_server: bool, session: &str) {
    let prefix = format!("socket<{socket}> ");
    let commands: Vec<&str> = calls
        .iter()
        .map(|call| {
            call.strip_prefix(&prefix)
                .unwrap_or_else(|| panic!("not run with {prefix}: {calls:#?}"))
        })
        .collect();
    let mut expected = vec![
        "worktree list --workspace w1 --json",
        "pane list --workspace w1",
        "plugin pane open ",
    ];
    if status_server {
        expected.insert(0, "status server");
    }
    assert_eq!(commands.len(), expected.len(), "{calls:#?}");
    for (command, expected) in commands.iter().zip(expected) {
        assert!(command.starts_with(expected), "{calls:#?}");
    }
    assert!(
        commands[commands.len() - 1].contains(&format!(" --env HERDR_SOCKET_PATH={session} ")),
        "{calls:#?}"
    );
}

#[test]
fn without_a_socket_every_command_inherits_none_and_the_session_is_the_server_s() {
    let f = Fixture::new();
    f.serve("/run/served.sock");
    assert_ran(&f.launch(None, None), "unset", true, "/run/served.sock");
}

#[test]
fn without_a_server_the_session_socket_is_default_and_no_command_is_given_it() {
    let f = Fixture::new();
    assert_ran(&f.launch(None, None), "unset", true, "default");
}

#[test]
fn an_inherited_socket_is_the_session_s_and_reaches_every_command() {
    let f = Fixture::new();
    f.serve("/run/served.sock");
    assert_ran(
        &f.launch(None, Some("/run/inherited.sock")),
        "/run/inherited.sock",
        false,
        "/run/inherited.sock",
    );
}

#[test]
fn the_socket_flag_wins_over_the_inherited_one_for_every_command() {
    let f = Fixture::new();
    f.serve("/run/served.sock");
    assert_ran(
        &f.launch(Some("/run/flag.sock"), Some("/run/inherited.sock")),
        "/run/flag.sock",
        false,
        "/run/flag.sock",
    );
}

/// The wake hook finds its space from its directory, and a `NAMEDROP_SPACE_ID`
/// it inherited under Herdr names none.
#[test]
fn the_mailbox_wake_hook_finds_its_space_on_the_inherited_socket() {
    for neutral in [None, Some("w9")] {
        let f = Fixture::new();
        let checkout = f.0.join("checkout");
        fs::create_dir(&checkout).unwrap();
        let checkout = checkout.canonicalize().unwrap();
        let mut command = f.namedrop(Some("/run/inherited.sock"));
        if let Some(space) = neutral {
            command.env("NAMEDROP_SPACE_ID", space);
        }
        let output = command
            .env("CHECKOUT", &checkout)
            .current_dir(&checkout)
            .args(["hook", "mailbox-wake", "--state-directory"])
            .arg(f.0.join("state"))
            .finish();
        let calls = f.calls();
        assert!(output.status.success(), "{output:?} {calls:#?}");
        assert_eq!(
            calls.first().map(String::as_str),
            Some(
                format!(
                    "socket</run/inherited.sock> worktree list --cwd {} --json",
                    checkout.display()
                )
                .as_str()
            ),
            "{calls:#?}"
        );
        assert!(
            calls.iter().all(|call| {
                call.starts_with("socket</run/inherited.sock> ") && !call.contains("w9")
            }),
            "{calls:#?}"
        );
    }
}

#[test]
fn a_neutral_endpoint_reaches_every_command_and_is_the_session_s() {
    let f = Fixture::new();
    f.serve("/run/served.sock");
    let mut command = f.namedrop(None);
    command.env("NAMEDROP_ENDPOINT", "/run/neutral.sock");
    let (output, calls) =
        f.launch_doctor(command, &["--herdr-workspace", "w1", "--herdr-pane", "p1"]);
    assert!(output.status.success(), "{output:?}");
    assert_ran(&calls, "/run/neutral.sock", false, "/run/neutral.sock");
}

#[test]
fn an_inherited_herdr_socket_outranks_a_neutral_endpoint() {
    let f = Fixture::new();
    let mut command = f.namedrop(Some("/run/inherited.sock"));
    command.env("NAMEDROP_ENDPOINT", "/run/neutral.sock");
    let (output, calls) =
        f.launch_doctor(command, &["--herdr-workspace", "w1", "--herdr-pane", "p1"]);
    assert!(output.status.success(), "{output:?}");
    assert_ran(&calls, "/run/inherited.sock", false, "/run/inherited.sock");
}

/// A Herdr pane's workspace or pane name, with no `HERDR_SOCKET_PATH`, still
/// means Herdr: every command inherits no socket, and the session is the
/// server's, not an inherited `NAMEDROP_ENDPOINT`.
#[test]
fn any_herdr_name_leaves_a_neutral_endpoint_unread() {
    let pane_flags: &[&str] = &["--herdr-workspace", "w1", "--herdr-pane", "p1"];
    for (herdr, value, flags) in [
        ("HERDR_WORKSPACE_ID", "w1", &pane_flags[2..]),
        ("HERDR_PANE_ID", "p1", pane_flags),
    ] {
        let f = Fixture::new();
        f.serve("/run/served.sock");
        let mut command = f.namedrop(None);
        command
            .env(herdr, value)
            .env("NAMEDROP_ENDPOINT", "/run/neutral.sock");
        let (output, calls) = f.launch_doctor(command, flags);
        assert!(output.status.success(), "{herdr} {output:?}");
        assert_ran(&calls, "unset", true, "/run/served.sock");
    }
}

#[test]
fn the_neutral_location_flags_spell_the_herdr_ones() {
    let f = Fixture::new();
    let mut command = f.namedrop(Some("/run/inherited.sock"));
    command.env("NAMEDROP_ENDPOINT", "/run/neutral.sock");
    let flags = [
        "--endpoint",
        "/run/flag.sock",
        "--space",
        "w1",
        "--pane",
        "p1",
    ];
    let (output, calls) = f.launch_doctor(command, &flags);
    assert!(output.status.success(), "{output:?}");
    assert_ran(&calls, "/run/flag.sock", false, "/run/flag.sock");

    let f = Fixture::new();
    let both = ["--endpoint", "/run/a.sock", "--herdr-socket", "/run/b.sock"];
    let (output, calls) = f.launch_doctor(f.namedrop(None), &both);
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("cannot be used multiple times"),
        "{output:?}"
    );
    assert!(calls.is_empty(), "{calls:#?}");
}

/// The space comes from `HERDR_WORKSPACE_ID` under Herdr, and from
/// `NAMEDROP_SPACE_ID` only where no Herdr name is set.
#[test]
fn a_space_comes_from_the_herdr_names_or_else_the_neutral_ones() {
    let first_call = |herdr: &[(&str, &str)], neutral: &[(&str, &str)]| {
        let f = Fixture::new();
        let mut command = f.namedrop(None);
        command
            .envs(herdr.iter().copied())
            .envs(neutral.iter().copied());
        let (_, calls) = f.launch_doctor(command, &["--herdr-pane", "p1"]);
        calls[0].clone()
    };
    let neutral = [
        ("NAMEDROP_ENDPOINT", "/run/neutral.sock"),
        ("NAMEDROP_SPACE_ID", "w1"),
    ];
    assert_eq!(
        first_call(&[], &neutral),
        "socket</run/neutral.sock> worktree list --workspace w1 --json"
    );
    let herdr = [
        ("HERDR_SOCKET_PATH", "/run/inherited.sock"),
        ("HERDR_WORKSPACE_ID", "w1"),
    ];
    assert_eq!(
        first_call(&herdr, &[("NAMEDROP_SPACE_ID", "w9")]),
        "socket</run/inherited.sock> worktree list --workspace w1 --json"
    );
    assert_eq!(
        first_call(&herdr[..1], &[("NAMEDROP_SPACE_ID", "w9")]),
        "socket</run/inherited.sock> pane list"
    );
}

/// An inherited pane is verified under the names in use, and the refusal
/// names that variable and its flag. The script `herdr` reports no shell for
/// the pane, so verification always refuses here.
#[test]
fn an_inherited_pane_is_verified_under_the_names_in_use() {
    let doctor = |envs: &[(&str, &str)]| {
        let f = Fixture::new();
        let mut command = f.namedrop(None);
        command.envs(envs.iter().copied());
        let output = command.args(["--herdr-workspace", "w1", "doctor"]).finish();
        let text = String::from_utf8_lossy(&output.stdout).into_owned()
            + &String::from_utf8_lossy(&output.stderr);
        (text, f.calls())
    };
    let (neutral, _) = doctor(&[
        ("NAMEDROP_ENDPOINT", "/run/neutral.sock"),
        ("NAMEDROP_PANE_ID", "p1"),
    ]);
    assert!(
        neutral.contains("NAMEDROP_PANE_ID=p1 was inherited") && neutral.contains("Pass --pane "),
        "{neutral}"
    );
    let (herdr, _) = doctor(&[
        ("HERDR_SOCKET_PATH", "/run/inherited.sock"),
        ("HERDR_PANE_ID", "p2"),
        ("NAMEDROP_PANE_ID", "p1"),
    ]);
    assert!(
        herdr.contains("HERDR_PANE_ID=p2 was inherited") && herdr.contains("Pass --herdr-pane "),
        "{herdr}"
    );
    let (mixed, calls) = doctor(&[
        ("HERDR_SOCKET_PATH", "/run/inherited.sock"),
        ("NAMEDROP_PANE_ID", "block:outer"),
    ]);
    assert!(mixed.contains(r#""status":"ok""#), "{mixed}");
    assert_eq!(
        calls,
        [
            "socket</run/inherited.sock> worktree list --workspace w1 --json",
            "socket</run/inherited.sock> pane list --workspace w1",
        ]
    );
}

#[test]
fn the_state_directory_follows_the_same_set_of_names_as_the_location() {
    let required = "--plugin-state, HERDR_PLUGIN_STATE_DIR or NAMEDROP_STATE_DIR is required";
    let launch = |herdr: Option<&str>, herdr_state: bool| {
        let f = Fixture::new();
        let neutral = f.0.join("neutral-state");
        let mut command = f.namedrop(herdr);
        command.env("NAMEDROP_STATE_DIR", &neutral);
        if herdr.is_none() {
            command.env("NAMEDROP_ENDPOINT", "/run/neutral.sock");
        }
        if !herdr_state {
            command.env_remove("HERDR_PLUGIN_STATE_DIR");
        }
        let (output, calls) = f.launch_doctor(command, &["--space", "w1", "--pane", "p1"]);
        (f, neutral, output, calls)
    };
    let popup_state = |calls: &[String], expected: &std::path::Path| {
        let popup = calls.last().unwrap();
        assert!(
            popup.contains(&format!(
                " --env HERDR_PLUGIN_STATE_DIR={} ",
                expected.display()
            )),
            "{calls:#?}"
        );
    };

    let (f, _, output, calls) = launch(Some("/run/inherited.sock"), true);
    assert!(output.status.success(), "{output:?}");
    popup_state(&calls, &f.0.join("state"));

    let (_, _, output, calls) = launch(Some("/run/inherited.sock"), false);
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout).contains(required),
        "{output:?}"
    );
    assert!(calls.is_empty(), "{calls:#?}");

    let (_, neutral, output, calls) = launch(None, false);
    assert!(output.status.success(), "{output:?}");
    popup_state(&calls, &neutral);
}

/// With no state directory named, a command refuses and runs no `herdr`,
/// inside Herdr and out: Namedrop has no default directory yet.
#[test]
fn without_a_state_directory_a_command_refuses_inside_herdr_and_out() {
    let required = "--plugin-state, HERDR_PLUGIN_STATE_DIR or NAMEDROP_STATE_DIR is required";
    for herdr in [false, true] {
        let f = Fixture::new();
        let mut command = f.namedrop(herdr.then_some("/run/inherited.sock"));
        command
            .env_remove("HERDR_PLUGIN_STATE_DIR")
            .env_remove("XDG_STATE_HOME");
        if !herdr {
            command.env("NAMEDROP_ENDPOINT", "/run/neutral.sock");
        }
        let (output, calls) = f.launch_doctor(command, &["--space", "w1", "--pane", "p1"]);
        assert!(!output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(required),
            "{output:?}"
        );
        assert!(calls.is_empty(), "{calls:#?}");
        assert!(!f.0.join("home/.local").exists());

        let output = f
            .namedrop(herdr.then_some("/run/inherited.sock"))
            .env_remove("HERDR_PLUGIN_STATE_DIR")
            .env_remove("XDG_STATE_HOME")
            .arg("dashboard-server")
            .finish();
        assert_eq!(output.status.code(), Some(64), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(required),
            "{output:?}"
        );
    }
}

#[test]
fn the_mailbox_wake_hook_runs_herdr_on_a_neutral_endpoint() {
    let f = Fixture::new();
    let checkout = f.0.join("checkout");
    fs::create_dir(&checkout).unwrap();
    let checkout = checkout.canonicalize().unwrap();
    let output = f
        .namedrop(None)
        .env("NAMEDROP_ENDPOINT", "/run/neutral.sock")
        .env("CHECKOUT", &checkout)
        .current_dir(&checkout)
        .args(["hook", "mailbox-wake", "--state-directory"])
        .arg(f.0.join("state"))
        .finish();
    let calls = f.calls();
    assert!(output.status.success(), "{output:?} {calls:#?}");
    assert!(!calls.is_empty());
    assert!(
        calls
            .iter()
            .all(|call| call.starts_with("socket</run/neutral.sock> ")),
        "{calls:#?}"
    );
}
