//! The one door through which a test names Namedrop's binary or starts a
//! program that may run it. A test run from a Rex block inherits that block's
//! claim, and a binary that saw it would take the block as its home and reach
//! the developer's own Rex server. A binary that inherited the developer's
//! `HOME` or `XDG_CONFIG_HOME` would write the install record, Herdr's
//! config, or agent packages into the developer's own. So nothing a test
//! starts inherits either.
#![allow(dead_code)]

use std::{ffi::OsStr, fs, path::PathBuf, process::Command, sync::OnceLock};

/// The names a Rex block gives its programs to say which block they run in.
const REX_CLAIM: [&str; 4] = ["REX_BLOCK", "REX_SESSION", "REX_CLIENT", "REX_SERVER"];

/// The roots of the developer's config and state that Namedrop reads in place
/// of one under `HOME`. Without them each falls back to the door's `HOME`.
const CONFIG_ROOTS: [&str; 7] = [
    "XDG_CONFIG_HOME",
    "XDG_STATE_HOME",
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "HERDR_CONFIG_PATH",
    "HERDR_PLUGIN_STATE_DIR",
    "NAMEDROP_STATE_DIR",
];

/// The binary's path, for a test that passes it on as data or to a program
/// that runs it. Start such a program with [`command`].
pub fn namedrop_path() -> &'static str {
    env!("CARGO_BIN_EXE_namedrop")
}

/// The binary, isolated as [`isolated`] says.
pub fn namedrop() -> Command {
    command(namedrop_path())
}

/// `program`, which may start the binary itself (a shell, Python, a hook
/// script, a copy of the binary, the test's own re-exec), isolated as
/// [`isolated`] says.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    isolated(&mut command);
    command
}

/// Removes the Rex claim and the config roots from a command built elsewhere,
/// and gives it the running test's own scratch `HOME`, which nothing reads
/// back. A test that needs its own home or root sets it on the command
/// afterwards, which replaces the door's.
pub fn isolated(command: &mut Command) -> &mut Command {
    for name in REX_CLAIM.into_iter().chain(CONFIG_ROOTS) {
        command.env_remove(name);
    }
    command.env("HOME", scratch_home())
}

/// `door/<pid>/<test>` under Cargo's directory for test files. Each test runs
/// on a thread named after it. The first call removes the homes of test
/// processes that have exited, which hold only what their programs left.
pub fn scratch_home() -> PathBuf {
    static SWEPT: OnceLock<()> = OnceLock::new();
    let door = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("door");
    SWEPT.get_or_init(|| {
        for entry in fs::read_dir(&door).into_iter().flatten().flatten() {
            let exited = entry
                .file_name()
                .to_str()
                .and_then(|pid| pid.parse::<libc::pid_t>().ok())
                .is_some_and(|pid| {
                    let probed = unsafe { libc::kill(pid, 0) };
                    probed == -1
                        && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                });
            if exited {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    });
    let thread = std::thread::current();
    door.join(std::process::id().to_string())
        .join(thread.name().unwrap_or("unnamed"))
}
