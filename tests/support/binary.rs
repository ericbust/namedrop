//! The one door through which a test names Namedrop's binary or starts a
//! program that may run it. A test run from a Rex block inherits that block's
//! claim, and a binary that saw it would take the block as its home and reach
//! the developer's own Rex server. So nothing a test starts inherits it.
#![allow(dead_code)]

use std::{ffi::OsStr, process::Command};

/// The names a Rex block gives its programs to say which block they run in.
const REX_CLAIM: [&str; 4] = ["REX_BLOCK", "REX_SESSION", "REX_CLIENT", "REX_SERVER"];

/// The binary's path, for a test that passes it on as data or to a program
/// that runs it. Start such a program with [`command`].
pub fn namedrop_path() -> &'static str {
    env!("CARGO_BIN_EXE_namedrop")
}

/// The binary, without the Rex claim.
pub fn namedrop() -> Command {
    command(namedrop_path())
}

/// `program`, which may start the binary itself (a shell, Python, a hook
/// script, a copy of the binary, the test's own re-exec), without the Rex
/// claim.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    without_rex(&mut command);
    command
}

/// Removes the Rex claim from a command built elsewhere.
pub fn without_rex(command: &mut Command) -> &mut Command {
    for name in REX_CLAIM {
        command.env_remove(name);
    }
    command
}
