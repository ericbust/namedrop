//! Minimal Unix liveness guard. It owns/reaps one CLI group, never the source PTY.
use std::{
    env, io,
    os::unix::process::CommandExt,
    process::{Command, ExitCode, Stdio},
};

pub(super) fn run(terminal: &str, rows: Option<u16>) -> ExitCode {
    let result = run_inner(terminal, rows);
    // Child diagnostics and terminal bytes must not enter dashboard logs.
    ExitCode::from(result.unwrap_or(1))
}

fn run_inner(terminal: &str, rows: Option<u16>) -> io::Result<u8> {
    if terminal.is_empty()
        || terminal.len() > 512
        || terminal.starts_with('-')
        || terminal.chars().any(char::is_control)
    {
        return Ok(1);
    }
    let socket =
        env::var_os("HERDR_SOCKET_PATH").ok_or_else(|| io::Error::other("missing socket"))?;
    // SAFETY: fstat writes to a valid stack allocation; fd 0 is borrowed stdin.
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(0, stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fstat succeeded above.
    if unsafe { stat.assume_init() }.st_mode & libc::S_IFMT != libc::S_IFIFO {
        return Ok(1);
    }
    let mut command = Command::new(env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into()));
    command.args(["terminal", "session", "observe", terminal]);
    if let Some(rows) = rows {
        command.arg("--rows").arg(rows.to_string());
    }
    let mut child = command
        .env("HERDR_SOCKET_PATH", socket)
        .env_remove("HERDR_PANE_ID")
        .env_remove("HERDR_TAB_ID")
        .env_remove("HERDR_WORKSPACE_ID")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::null())
        .spawn()?;
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                break Ok(status
                    .code()
                    .and_then(|code| u8::try_from(code).ok())
                    .unwrap_or(1));
            }
            Ok(None) => {}
            Err(error) => break Err(error),
        }
        let mut fd = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: poll borrows one valid pollfd. EOF/HUP on this private pipe
        // means our sole parent died or explicitly cancelled this observation.
        let ready = unsafe { libc::poll(&mut fd, 1, 50) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            break Err(error);
        }
        if ready > 0 {
            break Ok(0);
        }
    };
    crate::runtime::stop_process_group(&mut child);
    result
}
