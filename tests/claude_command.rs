//! The `claude_command` setting picks the program Namedrop runs for its own
//! Claude Code calls.
//!
//! Each case runs a real persona suggestion against fake programs on a
//! private `PATH`, and checks which program ran. The setting lives in the
//! process environment's `HOME`, so every case runs in one test, in order.

#[path = "support/cleanup.rs"]
mod cleanup;
#[path = "support/executable.rs"]
mod executable;

use cleanup::FixtureDirectory;
use namedrop::suggest_agent_persona;
use std::{
    env, fs,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};

/// A fake Claude Code that records its own name and answers one persona with
/// the envelope beside `record`.
fn fake(path: &Path, record: &Path) {
    let name = path.file_name().unwrap().to_str().unwrap();
    let response = record.with_file_name("response.json");
    executable::write_executable(
        path,
        format!(
            "#!/bin/sh\ncat >/dev/null\nprintf '%s\\n' '{name}' >> '{}'\ncat '{}'\n",
            record.display(),
            response.display()
        ),
    );
}

struct Fixture {
    root: FixtureDirectory,
    state: PathBuf,
    record: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("claude-command-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let bin = root.join("bin");
        let elsewhere = root.join("elsewhere");
        let state = root.join("state");
        for directory in [&bin, &elsewhere, &state, &root.join("home")] {
            fs::create_dir_all(directory).unwrap();
        }
        let record = root.join("record.txt");
        fs::write(
            root.join("response.json"),
            r#"{"type":"result","result":"{\"description\":\"Reviews code.\"}"}"#,
        )
        .unwrap();
        for program in [
            bin.join("claude"),
            bin.join("claude-personal"),
            bin.join("override"),
            elsewhere.join("launcher"),
        ] {
            fake(&program, &record);
        }
        // A file that is on PATH but cannot run.
        fs::write(bin.join("not-executable"), "#!/bin/sh\n").unwrap();
        // Safety: this binary has one test, so nothing reads the environment
        // while it changes.
        unsafe {
            env::set_var("HOME", root.join("home"));
            env::remove_var("XDG_CONFIG_HOME");
            env::remove_var("NAMEDROP_AGENT_STATUS_CLAUDE_BIN");
            // No real Claude Code is reachable: only the fakes and the system.
            env::set_var("PATH", format!("{}:/usr/bin:/bin", bin.display()));
        }
        Fixture {
            root: FixtureDirectory(root),
            state,
            record,
        }
    }

    fn herdr_settings(&self) -> PathBuf {
        self.root
            .0
            .join("home/.config/herdr/plugins/config/namedrop/settings.json")
    }

    fn write_settings(&self, path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    /// Runs one suggestion and returns its result and the programs that ran.
    fn suggest(&self) -> (Result<String, String>, String) {
        let _ = fs::remove_file(&self.record);
        let result = suggest_agent_persona(
            &self.state,
            "reviewer",
            "reviews code",
            "claude",
            &AtomicBool::new(false),
        );
        (result, fs::read_to_string(&self.record).unwrap_or_default())
    }
}

#[test]
fn claude_command_picks_the_program_for_namedrops_own_claude_calls() {
    let f = Fixture::new();
    let herdr = f.herdr_settings();

    // Absent: `claude` on PATH, as before the setting existed.
    let (result, ran) = f.suggest();
    assert_eq!(result.as_deref(), Ok("Reviews code."), "absent: {ran}");
    assert_eq!(ran, "claude\n", "absent");
    f.write_settings(&herdr, r#"{"check_for_updates":false}"#);
    assert_eq!(f.suggest().1, "claude\n", "a file without the setting");

    // A program name is looked up on PATH.
    f.write_settings(&herdr, r#"{"claude_command":"claude-personal"}"#);
    let (result, ran) = f.suggest();
    assert_eq!(result.as_deref(), Ok("Reviews code."), "name: {ran}");
    assert_eq!(ran, "claude-personal\n", "name");

    // An absolute path runs even when it is not on PATH.
    let launcher = f.root.0.join("elsewhere/launcher");
    f.write_settings(
        &herdr,
        &format!(r#"{{"claude_command":"{}"}}"#, launcher.display()),
    );
    let (result, ran) = f.suggest();
    assert_eq!(result.as_deref(), Ok("Reviews code."), "absolute: {ran}");
    assert_eq!(ran, "launcher\n", "absolute");

    // An invalid value fails with its reason and never falls back to `claude`.
    for (settings, reason) in [
        (
            r#"{"claude_command":""}"#.to_owned(),
            "claude_command in settings.json is empty",
        ),
        (
            r#"{"claude_command":"missing-launcher"}"#.to_owned(),
            "claude_command missing-launcher is not on PATH",
        ),
        (
            r#"{"claude_command":"not-executable"}"#.to_owned(),
            "claude_command not-executable is not on PATH",
        ),
        (
            r#"{"claude_command":"bin/claude-personal"}"#.to_owned(),
            "must be a program name or an absolute path",
        ),
        (
            format!(
                r#"{{"claude_command":"{}"}}"#,
                f.root.0.join("elsewhere").display()
            ),
            "is not an executable file",
        ),
        (
            r#"{"claude_command":7}"#.to_owned(),
            "settings.json is invalid",
        ),
        (
            r#"{"claude_command":["claude-personal"]}"#.to_owned(),
            "settings.json is invalid",
        ),
        ("{".to_owned(), "settings.json is invalid"),
    ] {
        f.write_settings(&herdr, &settings);
        let (result, ran) = f.suggest();
        let error = result.expect_err(&settings);
        assert!(error.contains(reason), "{settings}: {error}");
        assert_eq!(ran, "", "{settings} must run nothing");
    }

    // The test hook wins over the setting.
    f.write_settings(&herdr, r#"{"claude_command":"claude-personal"}"#);
    // Safety: as in `Fixture::new`.
    unsafe {
        env::set_var(
            "NAMEDROP_AGENT_STATUS_CLAUDE_BIN",
            f.root.0.join("bin/override"),
        )
    };
    assert_eq!(f.suggest().1, "override\n", "the test hook");
    unsafe { env::remove_var("NAMEDROP_AGENT_STATUS_CLAUDE_BIN") };

    // An install that `install.sh` made reads its own settings file.
    fs::create_dir_all(f.state.join("runtime")).unwrap();
    fs::write(f.state.join("runtime/owner"), "standalone\n").unwrap();
    f.write_settings(
        &f.root.0.join("home/.config/namedrop/settings.json"),
        &format!(r#"{{"claude_command":"{}"}}"#, launcher.display()),
    );
    assert_eq!(f.suggest().1, "launcher\n", "standalone");
}
