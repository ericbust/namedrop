//! Writes the executables a test then runs.
//!
//! A test binary runs its tests on threads, and many of them spawn processes.
//! A spawned child holds a copy of every descriptor its parent has open until
//! it execs, close-on-exec ones included. While that copy of a write
//! descriptor exists, the kernel refuses to run the file (ETXTBSY), so a test
//! that writes a script and runs it can fail because another test spawned
//! something in between. Here the file is opened for writing only in a child
//! process, where no spawn of this process can copy the descriptor, and the
//! child has exited before the file is run.

use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

/// Writes `contents` to `path` and makes it executable.
pub fn write_executable(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) {
    let path = path.as_ref();
    let mut writer = Command::new("/bin/sh")
        .args(["-c", r#"cat > "$1" && chmod 755 "$1""#, "write-executable"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    writer
        .stdin
        .take()
        .unwrap()
        .write_all(contents.as_ref())
        .unwrap();
    assert!(
        writer.wait().unwrap().success(),
        "could not write {}",
        path.display()
    );
}
