#![cfg(feature = "test-support")]

#[path = "support/binary.rs"]
mod binary;
#[path = "support/cleanup.rs"]
mod cleanup;

use namedrop::{
    AgentConfigDirectories, LifecycleRuntime, ProcessIds, StateNamespace, StateStore, SystemClock,
    activate_from_naming_screen, execute_lifecycle_operation,
    mux::{Effect, fake::FakeMux},
};
use std::{
    fs,
    io::{Read, Write},
    path::PathBuf,
    process::Stdio,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "nd-rex-popup-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("home")).unwrap();
        Self(root)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        cleanup::remove_fixture(&self.0).unwrap();
    }
}

#[test]
fn naming_activation_keeps_the_tour_unclaimed_and_naming_continues() {
    let f = Fixture::new();
    let home = f.0.join("home");
    let state = f.0.join("state");
    let mux = FakeMux::world()
        .endpoint("rex:/tmp/rex-popup.sock")
        .space("s1")
        .pane("p1")
        .terminal("t1")
        .tab("window:w")
        .claude()
        .session("agent-1")
        .idle()
        .build();
    let ids = ProcessIds::default();
    let runtime = LifecycleRuntime {
        state_directory: &state,
        home_directory: Some(&home),
        agent_config: AgentConfigDirectories::default(),
        cwd: &f.0,
        workspace_id: Some("s1"),
        current_pane_id: Some("p1"),
        session_socket_id: "rex:/tmp/rex-popup.sock",
        mux: &mux,
        clock: &SystemClock,
        ids: &ids,
    };
    let activated = activate_from_naming_screen(&runtime).unwrap();
    assert_eq!(activated["activated"], true);
    assert_eq!(activated["tour"]["shown"], false);
    assert!(!namedrop::tour::is_shown(&state, namedrop::tour::CURRENT_ID).unwrap());
    assert!(
        !mux.attempted_effects()
            .iter()
            .any(|e| matches!(e, Effect::OpenPopup(_)))
    );
    let namespace = StateNamespace::for_space(runtime.session_socket_id, "s1");
    let mut store = StateStore::open(&state).unwrap();
    assert!(
        store
            .transaction(|tx| tx.worktree_state(&namespace))
            .unwrap()
            .unwrap()
            .activated
    );
    let named = execute_lifecycle_operation(
        "name",
        serde_json::json!({"alias":"rex-agent", "pane_type":"agent"}),
        &runtime,
    )
    .unwrap();
    assert_eq!(named["alias"], "@rex-agent");
    assert!(!namedrop::tour::is_shown(&state, namedrop::tour::CURRENT_ID).unwrap());
}

/// Runs one Rex screen whose start is refused, and checks that it prints
/// `refusal`, waits for a key even after a second, and closes on one. With
/// `gate`, the runtime command lock must be free for an exclusive taker, such
/// as an update, while it waits.
fn waits_for_one_key(
    mut command: std::process::Command,
    mode: &str,
    refusal: &str,
    gate: Option<&std::path::Path>,
) {
    let mut child = command
        .args(["--endpoint", "rex:/tmp/nd-rex-popup-does-not-exist.sock"])
        .args(["ui", "run", mode])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let (sent, received) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let mut byte = [0];
        while stderr.read(&mut byte).unwrap() != 0 {
            text.push(byte[0] as char);
            if text.ends_with("Press any key to close: ") {
                sent.send(()).unwrap();
            }
        }
        text
    });
    received
        .recv_timeout(Duration::from_secs(10))
        .unwrap_or_else(|_| panic!("{mode}: no key prompt was printed"));
    assert!(
        child.try_wait().unwrap().is_none(),
        "{mode} exited before a key"
    );
    std::thread::sleep(Duration::from_secs(1));
    assert!(
        child.try_wait().unwrap().is_none(),
        "{mode} closed a slow failure"
    );
    if let Some(gate) = gate {
        let lock = fs::File::open(gate).unwrap();
        assert!(
            lock.try_lock().is_ok(),
            "{mode} holds the command gate while it waits"
        );
    }
    input.write_all(b"x").unwrap();
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{mode}");
    let error = reader.join().unwrap();
    assert!(error.contains(refusal), "{mode}: {error}");
}

fn untargeted(f: &Fixture) -> std::process::Command {
    let mut command = binary::namedrop();
    command
        .env("HOME", f.0.join("home"))
        .env_remove("NAMEDROP_TARGET_WORKSPACE")
        .env_remove("NAMEDROP_TARGET_PANE")
        .env_remove("NAMEDROP_TARGET_TERMINAL");
    command
}

#[test]
fn rex_screen_refusals_wait_for_any_key_even_after_one_second() {
    for mode in ["name", "manage", "doctor", "tour", "status"] {
        let f = Fixture::new();
        waits_for_one_key(
            untargeted(&f),
            mode,
            "This popup is missing its invoking space",
            None,
        );
        assert!(!f.0.join("home/.config").exists());
    }
}

/// Breaks a state's `runtime` directory one way.
type Break = fn(&std::path::Path);

/// A runtime command gate that refuses, or a selected runtime that cannot
/// start, ends the process before any screen runs. A Rex layer still waits
/// for its key, without the gate, and Herdr's popup keeps today's exit.
#[test]
fn refusals_before_a_screen_starts_wait_for_a_key_only_in_rex() {
    let cases: [(&str, Break, &str); 2] = [
        (
            "gate",
            |runtime| fs::create_dir_all(runtime.join("commands.lock")).unwrap(),
            "runtime command lock is not a regular file",
        ),
        (
            "runtime",
            |runtime| fs::write(runtime.join("namedrop"), "not executable").unwrap(),
            "Could not run the selected Namedrop runtime",
        ),
    ];
    for (case, break_runtime, refusal) in cases {
        for mode in ["name", "status"] {
            let f = Fixture::new();
            let state = f.0.join("state");
            fs::create_dir_all(state.join("runtime")).unwrap();
            break_runtime(&state.join("runtime"));
            let mut rex = untargeted(&f);
            rex.env("NAMEDROP_STATE_DIR", &state);
            let gate = state.join("runtime/commands.lock");
            waits_for_one_key(rex, mode, refusal, (case == "runtime").then_some(&*gate));
            let output = untargeted(&f)
                .env("HERDR_SOCKET_PATH", f.0.join("herdr.sock"))
                .env("HERDR_PLUGIN_STATE_DIR", &state)
                .args(["ui", "run", mode])
                .stdin(Stdio::null())
                .output()
                .unwrap();
            let error = String::from_utf8_lossy(&output.stderr);
            assert_eq!(output.status.code(), Some(69), "{case} {mode}: {error}");
            assert!(error.contains(refusal), "{case} {mode}: {error}");
            assert!(!error.contains("Press any key"), "{case} {mode}: {error}");
        }
    }
}

/// Only `ui run` is a screen a person closes. On a Rex home, `ui launch` and
/// every other command refused before they start exit 69 at once.
#[test]
fn refusals_before_any_other_rex_command_exit_69_without_a_key_wait() {
    let cases: [(&str, Break, &str); 2] = [
        (
            "gate",
            |runtime| fs::write(runtime.join("migration.json"), "{}").unwrap(),
            "Namedrop activation was interrupted",
        ),
        (
            "runtime",
            |runtime| fs::write(runtime.join("namedrop"), "not executable").unwrap(),
            "Could not run the selected Namedrop runtime",
        ),
    ];
    for (case, break_runtime, refusal) in cases {
        for command in [&["ui", "launch", "name"][..], &["address", "list"]] {
            let f = Fixture::new();
            let state = f.0.join("state");
            fs::create_dir_all(state.join("runtime")).unwrap();
            break_runtime(&state.join("runtime"));
            let mut child = untargeted(&f)
                .env("NAMEDROP_STATE_DIR", &state)
                .args(["--endpoint", "rex:/tmp/nd-rex-popup-does-not-exist.sock"])
                .args(command)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            // A key is ready, so a command that waited would close on it
            // with 0. One that exited first has closed the pipe.
            let _ = child.stdin.take().unwrap().write_all(b"x");
            let output = child.wait_with_output().unwrap();
            let error = String::from_utf8_lossy(&output.stderr);
            assert_eq!(
                output.status.code(),
                Some(69),
                "{case} {command:?}: {error}"
            );
            assert!(error.contains(refusal), "{case} {command:?}: {error}");
            assert!(
                !error.contains("Press any key"),
                "{case} {command:?}: {error}"
            );
        }
    }
}

/// Herdr closes its own popups, so a Herdr screen's error keeps today's exit
/// code and never waits for the Rex key.
#[test]
fn herdr_screen_errors_exit_65_without_the_rex_key_wait() {
    for mode in ["name", "manage", "doctor", "tour", "status"] {
        let f = Fixture::new();
        let mut child = binary::namedrop()
            .env("HOME", f.0.join("home"))
            .env("HERDR_SOCKET_PATH", f.0.join("herdr.sock"))
            .env("HERDR_PANE_ID", "popup-pane")
            .env("HERDR_PLUGIN_STATE_DIR", f.0.join("state"))
            .env_remove("NAMEDROP_TARGET_WORKSPACE")
            .env_remove("NAMEDROP_TARGET_PANE")
            .env_remove("NAMEDROP_TARGET_TERMINAL")
            .args(["ui", "run", mode])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"x\n").unwrap();
        let output = child.wait_with_output().unwrap();
        let error = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(65), "{mode}: {error}");
        assert!(
            error.contains("This popup is missing its invoking space"),
            "{mode}: {error}"
        );
        assert!(!error.contains("Press any key"), "{mode}: {error}");
    }
}
