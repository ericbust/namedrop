//! Which Herdr server a command talks to. Every `herdr` the CLI starts
//! inherits `HERDR_SOCKET_PATH` unless `--herdr-socket` names one, and the
//! session's socket is the flag's, the environment's, or the one `status
//! server` reports, in that order. The mailbox wake hook takes no flag, so
//! its `herdr` inherits the variable too.
//!
//! Each location also has a neutral name (`namedrop::locator`): a flag beside
//! each `--herdr-*` one and a `NAMEDROP_*` variable that only counts where
//! the Herdr variable is unset.

#[path = "support/executable.rs"]
mod executable;

use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

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
            .output()
            .unwrap();
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
            .output()
            .unwrap();
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

#[test]
fn the_mailbox_wake_hook_finds_its_space_on_the_inherited_socket() {
    let f = Fixture::new();
    let checkout = f.0.join("checkout");
    fs::create_dir(&checkout).unwrap();
    let checkout = checkout.canonicalize().unwrap();
    let output = f
        .namedrop(Some("/run/inherited.sock"))
        .env("CHECKOUT", &checkout)
        .current_dir(&checkout)
        .args(["hook", "mailbox-wake", "--state-directory"])
        .arg(f.0.join("state"))
        .output()
        .unwrap();
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
        calls
            .iter()
            .all(|call| call.starts_with("socket</run/inherited.sock> ")),
        "{calls:#?}"
    );
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

#[test]
fn a_space_comes_from_the_herdr_name_then_the_neutral_one() {
    let workspace = |herdr: Option<&str>, neutral: &str| {
        let f = Fixture::new();
        let mut command = f.namedrop(Some("/run/inherited.sock"));
        command.env("NAMEDROP_SPACE_ID", neutral);
        if let Some(space) = herdr {
            command.env("HERDR_WORKSPACE_ID", space);
        }
        let (_, calls) = f.launch_doctor(command, &["--herdr-pane", "p1"]);
        calls[0].clone()
    };
    assert_eq!(
        workspace(None, "w1"),
        "socket</run/inherited.sock> worktree list --workspace w1 --json"
    );
    assert_eq!(
        workspace(Some("w1"), "w9"),
        "socket</run/inherited.sock> worktree list --workspace w1 --json"
    );
}

/// An inherited pane must be verified whichever name held it, and the
/// refusal names that variable and its flag. The script `herdr` reports no
/// shell for the pane, so verification always refuses here.
#[test]
fn an_inherited_pane_is_verified_under_the_name_that_held_it() {
    let refusal = |herdr: Option<&str>, neutral: &str| {
        let f = Fixture::new();
        let mut command = f.namedrop(Some("/run/inherited.sock"));
        command.env("NAMEDROP_PANE_ID", neutral);
        if let Some(pane) = herdr {
            command.env("HERDR_PANE_ID", pane);
        }
        let (output, calls) = f.launch_doctor(command, &["--herdr-workspace", "w1"]);
        assert!(!output.status.success(), "{output:?}");
        assert!(
            calls.iter().all(|call| !call.contains("plugin pane open")),
            "{calls:#?}"
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
            + &String::from_utf8_lossy(&output.stderr)
    };
    let neutral = refusal(None, "p1");
    assert!(
        neutral.contains("NAMEDROP_PANE_ID=p1 was inherited") && neutral.contains("Pass --pane "),
        "{neutral}"
    );
    let herdr = refusal(Some("p2"), "p1");
    assert!(
        herdr.contains("HERDR_PANE_ID=p2 was inherited") && herdr.contains("Pass --herdr-pane "),
        "{herdr}"
    );
}

#[test]
fn the_state_directory_comes_from_the_herdr_name_then_the_neutral_one() {
    let state_of = |herdr: bool| {
        let f = Fixture::new();
        let neutral = f.0.join("neutral-state");
        let mut command = f.namedrop(Some("/run/inherited.sock"));
        command.env("NAMEDROP_STATE_DIR", &neutral);
        if !herdr {
            command.env_remove("HERDR_PLUGIN_STATE_DIR");
        }
        let (output, calls) =
            f.launch_doctor(command, &["--herdr-workspace", "w1", "--herdr-pane", "p1"]);
        assert!(output.status.success(), "{output:?}");
        let expected = if herdr { f.0.join("state") } else { neutral };
        let popup = calls.last().unwrap();
        assert!(
            popup.contains(&format!(
                " --env HERDR_PLUGIN_STATE_DIR={} ",
                expected.display()
            )),
            "{calls:#?}"
        );
    };
    state_of(true);
    state_of(false);
}

/// With no state directory named, a command outside Herdr uses Namedrop's
/// own default, and one under Herdr still refuses, because Herdr's plugin
/// directory is the only one its panes share.
#[test]
fn the_default_state_directory_is_used_only_where_herdr_names_none() {
    let f = Fixture::new();
    let mut command = f.namedrop(None);
    command
        .env_remove("HERDR_PLUGIN_STATE_DIR")
        .env_remove("XDG_STATE_HOME")
        .env("NAMEDROP_ENDPOINT", "/run/neutral.sock");
    let (output, calls) = f.launch_doctor(command, &["--space", "w1", "--pane", "p1"]);
    assert!(output.status.success(), "{output:?}");
    let default = f.0.join("home/.local/state/namedrop");
    assert!(
        calls.last().unwrap().contains(&format!(
            " --env HERDR_PLUGIN_STATE_DIR={} ",
            default.display()
        )),
        "{calls:#?}"
    );

    let f = Fixture::new();
    let mut command = f.namedrop(Some("/run/inherited.sock"));
    command.env_remove("HERDR_PLUGIN_STATE_DIR");
    let (output, calls) = f.launch_doctor(command, &["--space", "w1", "--pane", "p1"]);
    assert!(!output.status.success(), "{output:?}");
    assert!(
        String::from_utf8_lossy(&output.stdout)
            .contains("--plugin-state, HERDR_PLUGIN_STATE_DIR or NAMEDROP_STATE_DIR is required"),
        "{output:?}"
    );
    assert!(calls.is_empty(), "{calls:#?}");
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
        .output()
        .unwrap();
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
