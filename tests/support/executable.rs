//! Publishes the executables a test then runs.
//!
//! A test binary runs its tests on threads, and many of them spawn processes.
//! A spawned child holds a copy of every descriptor its parent has open until
//! it execs, close-on-exec ones included. While that copy of a write
//! descriptor exists, the kernel refuses to run the file (ETXTBSY), so a test
//! that writes a script and runs it can fail because another test spawned
//! something in between. Here the file is opened for writing only in a child
//! process, where no spawn of this process can copy the descriptor, and the
//! child has exited before the file is run.
//!
//! The child fills a new file beside the path and renames it into place, so a
//! path whose previous executable is still running can be replaced too.

#![allow(dead_code)]

use std::{
    io::Write,
    path::Path,
    process::{Child, Command, Stdio},
};

/// Publishes `contents` at `path` as an executable.
pub fn write_executable(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) {
    let path = path.as_ref();
    let mut writer = publisher(r#"cat > "$staged" && chmod 755 "$staged""#, path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    writer
        .stdin
        .take()
        .unwrap()
        .write_all(contents.as_ref())
        .unwrap();
    published(writer, path);
}

/// Publishes a copy of `source` at `path`, with the mode `source` has.
pub fn copy_executable(source: impl AsRef<Path>, path: impl AsRef<Path>) {
    let path = path.as_ref();
    let copier = publisher(r#"cp "$2" "$staged""#, path)
        .arg(source.as_ref())
        .spawn()
        .unwrap();
    published(copier, path);
}

fn publisher(fill: &str, path: &Path) -> Command {
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(format!(
            r#"staged="$1.staged.$$" && {fill} && mv -f "$staged" "$1""#
        ))
        .arg("publish-executable")
        .arg(path);
    command
}

fn published(mut publisher: Child, path: &Path) {
    assert!(
        publisher.wait().unwrap().success(),
        "could not publish {}",
        path.display()
    );
}
