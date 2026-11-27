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
            "#!/bin/sh\n/bin/cat >/dev/null\nprintf '%s\\n' '{name}' >> '{}'\n/bin/cat '{}'\n",
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
            bin.join("claude-launcher"),
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

    let local_bin = f.root.0.join("home/.local/bin");
    fs::create_dir_all(&local_bin).unwrap();
    let native_claude = f.root.0.join("elsewhere/native-claude");
    fake(&native_claude, &f.record);
    std::os::unix::fs::symlink(&native_claude, local_bin.join("claude")).unwrap();
    let owner_path = f.state.join("runtime/owner");
    fs::create_dir_all(owner_path.parent().unwrap()).unwrap();
    let full_path = env::var_os("PATH").unwrap();
    // Model a screen whose PATH has no Claude Code executable. Keep the search
    // inside the fixture so this can never pick up another Claude Code install.
    // Safety: this test is the only test in the binary.
    unsafe { env::set_var("PATH", &f.state) };
    for owner in ["standalone", "herdr"] {
        fs::write(&owner_path, owner).unwrap();
        let (result, ran) = f.suggest();
        assert_eq!(result.as_deref(), Ok("Reviews code."), "{owner}: {ran}");
        assert_eq!(ran, "native-claude\n", "{owner}");
    }
    fs::remove_file(owner_path).unwrap();
    // Safety: this test is the only test in the binary.
    unsafe { env::set_var("PATH", full_path) };
    assert_eq!(f.suggest().1, "claude\n", "PATH takes precedence");

    f.write_settings(&herdr, r#"{"check_for_updates":false}"#);
    assert_eq!(f.suggest().1, "claude\n", "a file without the setting");

    // A program name is looked up on PATH.
    f.write_settings(&herdr, r#"{"claude_command":"claude-launcher"}"#);
    let (result, ran) = f.suggest();
    assert_eq!(result.as_deref(), Ok("Reviews code."), "name: {ran}");
    assert_eq!(ran, "claude-launcher\n", "name");

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
            "claude_command is empty",
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
            r#"{"claude_command":"bin/claude-launcher"}"#.to_owned(),
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
            r#"{"claude_command":null}"#.to_owned(),
            "settings.json is invalid",
        ),
        (
            r#"{"claude_command":["claude-launcher"]}"#.to_owned(),
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

    // The test hook wins over the setting, and is checked the same way.
    f.write_settings(&herdr, r#"{"claude_command":"claude-launcher"}"#);
    let hook = |value: &Path| {
        // Safety: as in `Fixture::new`.
        unsafe { env::set_var("NAMEDROP_AGENT_STATUS_CLAUDE_BIN", value) };
        let suggested = f.suggest();
        unsafe { env::remove_var("NAMEDROP_AGENT_STATUS_CLAUDE_BIN") };
        suggested
    };
    assert_eq!(
        hook(&f.root.0.join("bin/override")).1,
        "override\n",
        "the test hook"
    );
    let (result, ran) = hook(&f.root.0.join("bin/missing"));
    let error = result.expect_err("a missing test hook program");
    assert!(
        error.contains("NAMEDROP_AGENT_STATUS_CLAUDE_BIN")
            && error.contains("is not an executable file"),
        "{error}"
    );
    assert_eq!(ran, "", "a missing test hook program must run nothing");

    // A claude_command only in a file this install does not read fails, and
    // names both files.
    let standalone = f.root.0.join("home/.config/namedrop/settings.json");
    fs::remove_file(&herdr).unwrap();
    f.write_settings(&standalone, r#"{"claude_command":"claude-launcher"}"#);
    let (result, ran) = f.suggest();
    let error = result.expect_err("a claude_command in the unread file");
    assert!(
        error.contains(&format!(
            "claude_command is set in {}, but this install reads {}; move it to",
            standalone.display(),
            herdr.display()
        )),
        "{error}"
    );
    assert_eq!(ran, "", "an unread claude_command must run nothing");
    // The same value in both files is no conflict.
    f.write_settings(&herdr, r#"{"claude_command":"claude-launcher"}"#);
    assert_eq!(f.suggest().1, "claude-launcher\n", "the same value in both");
    fs::remove_file(&standalone).unwrap();
    // Herdr's file under `~/.config` while this process reads `XDG_CONFIG_HOME`.
    unsafe { env::set_var("XDG_CONFIG_HOME", f.root.0.join("xdg")) };
    let (result, ran) = f.suggest();
    let error = result.expect_err("a claude_command beside XDG_CONFIG_HOME");
    assert!(error.contains("claude_command is set in"), "{error}");
    assert!(
        error.contains(&f.root.0.join("xdg").display().to_string()),
        "{error}"
    );
    assert_eq!(ran, "", "XDG_CONFIG_HOME");
    unsafe { env::remove_var("XDG_CONFIG_HOME") };

    // A value set differently in both files fails, and says how to settle it.
    f.write_settings(&herdr, r#"{"claude_command":"claude-launcher"}"#);
    f.write_settings(
        &standalone,
        &format!(r#"{{"claude_command":"{}"}}"#, launcher.display()),
    );
    let (result, ran) = f.suggest();
    let error = result.expect_err("claude_command set differently in both files");
    assert!(
        error.contains(&format!(
            "claude_command is set in {} and in {}, which this install reads, to different programs; make them match, or remove it from {}",
            standalone.display(),
            herdr.display(),
            standalone.display()
        )),
        "{error}"
    );
    assert_eq!(ran, "", "a conflicting claude_command must run nothing");
    // A file this install does not read that cannot be parsed might set it,
    // so nothing runs.
    f.write_settings(&standalone, r#"{"claude_command":"claude-launcher",}"#);
    let (result, ran) = f.suggest();
    let error = result.expect_err("an unreadable unread file");
    assert!(
        error.contains(&format!(
            "{} is invalid, so Namedrop cannot tell whether it sets claude_command",
            standalone.display()
        )),
        "{error}"
    );
    assert_eq!(ran, "", "an unreadable unread file must run nothing");
    fs::remove_file(&standalone).unwrap();
    // A settings link that leads nowhere is not a missing file, whichever
    // file it is.
    for link in [&herdr, &standalone] {
        let _ = fs::remove_file(link);
        std::os::unix::fs::symlink(f.root.0.join("nowhere.json"), link).unwrap();
        let (result, ran) = f.suggest();
        let error = result.expect_err("a dangling settings link");
        assert!(
            error.contains(&format!("{} is invalid", link.display())),
            "{error}"
        );
        assert_eq!(ran, "", "a dangling settings link must run nothing");
        fs::remove_file(link).unwrap();
    }
    // So is a settings file under a directory link that leads nowhere, as
    // when ~/.config/herdr links to a dotfiles directory that is gone.
    let herdr_config = f.root.0.join("home/.config/herdr");
    let moved = f.root.0.join("herdr-config-moved");
    fs::rename(&herdr_config, &moved).unwrap();
    std::os::unix::fs::symlink(f.root.0.join("gone-dotfiles"), &herdr_config).unwrap();
    let (result, ran) = f.suggest();
    let error = result.expect_err("a dangling directory link above the settings file");
    assert!(
        error.contains(&format!(
            "{} is a link that leads nowhere",
            herdr_config.display()
        )),
        "{error}"
    );
    assert_eq!(ran, "", "a dangling directory link must run nothing");
    fs::remove_file(&herdr_config).unwrap();
    fs::rename(&moved, &herdr_config).unwrap();
    f.write_settings(&herdr, r#"{"claude_command":"claude-launcher"}"#);

    // An owner that cannot be read leaves the file unknown, so nothing runs.
    fs::create_dir_all(f.state.join("runtime")).unwrap();
    fs::write(f.state.join("runtime/owner"), "bogus\n").unwrap();
    let (result, ran) = f.suggest();
    let error = result.expect_err("an unreadable owner");
    assert!(error.contains("cannot tell which settings.json"), "{error}");
    assert_eq!(ran, "", "an unreadable owner must run nothing");

    // An install that `install.sh` made reads its own settings file.
    fs::remove_file(&herdr).unwrap();
    fs::write(f.state.join("runtime/owner"), "standalone\n").unwrap();
    f.write_settings(
        &standalone,
        &format!(r#"{{"claude_command":"{}"}}"#, launcher.display()),
    );
    assert_eq!(f.suggest().1, "launcher\n", "standalone");
}
