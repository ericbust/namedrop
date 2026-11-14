//! The relay's `rex` calls. Each passes `--autostart=false` and runs through
//! `daemon::command`, so no inherited block, session, or client picks its
//! target, and tailnet TLS and Rex's admission stay Rex's own code.

use crate::daemon::{self, Scope, Starter};
use std::{
    fmt,
    io::{Read, Write},
    path::PathBuf,
    process::Stdio,
    thread,
    time::{Duration, Instant},
};

/// How long one `rex` process may run.
pub(super) const CALL: Duration = Duration::from_secs(15);

#[derive(Debug)]
pub(super) enum Failure {
    /// `rex` could not start.
    Start(String),
    /// `rex` ran past its deadline and was killed.
    Deadline(Duration),
    /// `rex` exited unsuccessfully, with what it wrote to stderr.
    Exit(String),
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Start(error) => write!(formatter, "rex did not start: {error}"),
            Self::Deadline(within) => write!(formatter, "rex ran past {} s", within.as_secs()),
            Self::Exit(stderr) => formatter.write_str(stderr.trim()),
        }
    }
}

#[cfg(not(test))]
fn program() -> PathBuf {
    PathBuf::from("rex")
}

#[cfg(test)]
fn program() -> PathBuf {
    super::tests_support::program()
}

/// Runs `rex --autostart=false <args>`, with `input` on stdin, and returns
/// its stdout.
pub(super) fn run(
    args: &[&str],
    input: Option<&[u8]>,
    within: Duration,
) -> Result<Vec<u8>, Failure> {
    let mut command = daemon::command(&program(), Scope::None, Starter::Caller);
    command
        .arg("--autostart=false")
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| Failure::Start(error.to_string()))?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        let input = input.to_vec();
        thread::spawn(move || stdin.write_all(&input));
    }
    let read = |pipe: Option<Box<dyn Read + Send>>| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = read(
        child
            .stdout
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    );
    let stderr = read(
        child
            .stderr
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    );
    let deadline = Instant::now() + within;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Failure::Deadline(within));
            }
        }
    };
    let stdout = stdout.join().unwrap_or_default();
    let stderr = stderr.join().unwrap_or_default();
    if status.success() {
        Ok(stdout)
    } else {
        Err(Failure::Exit(String::from_utf8_lossy(&stderr).into_owned()))
    }
}
