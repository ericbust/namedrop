use crate::{Clock, IdGenerator};
use std::{
    env, fs,
    io::{Read, Seek, SeekFrom},
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

static NEXT_OUTPUT_FILE: AtomicU64 = AtomicU64::new(1);

/// The terminal `ps` names as `pid`'s controlling TTY, or `None` when `ps`
/// fails, writes more than 1 KiB to either stream, or outlives `timeout`.
pub(crate) fn controlling_tty(pid: u64, timeout: Duration) -> Option<String> {
    let mut ps = Command::new("/bin/ps");
    ps.args(["-p", &pid.to_string(), "-o", "tty="]);
    short_answer(ps, timeout)
}

/// `command`'s trimmed stdout, or `None` when it fails, writes more than
/// 1 KiB to either stream, or outlives `timeout`.
fn short_answer(command: Command, timeout: Duration) -> Option<String> {
    let output = output_bounded(command, timeout, &AtomicBool::new(false), 1024).ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|answer| answer.trim().to_owned())
}

/// Runs `command` in a process group of its own with stdin closed, and stops
/// the group once `cancelled` is set, `timeout` passes, or either stream
/// passes `max_output_bytes`. Output goes to unlinked private files, not
/// pipes. On macOS a child that another thread starts while a new pipe is
/// still inheritable keeps its write end open, so reading a pipe to its end
/// waits for that child. The error texts are the ones the Herdr adapter
/// classifies.
pub(crate) fn output_bounded(
    mut command: Command,
    timeout: Duration,
    cancelled: &AtomicBool,
    max_output_bytes: usize,
) -> Result<Output, String> {
    if cancelled.load(Ordering::Relaxed) {
        return Err("Herdr read cancelled".to_owned());
    }
    let program = command.get_program().to_string_lossy().into_owned();
    let stdout = output_file("stdout")?;
    let stderr = output_file("stderr")?;
    let mut child = command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout.try_clone().map_err(|error| {
            format!("failed to capture Herdr command stdout: {error}")
        })?))
        .stderr(Stdio::from(stderr.try_clone().map_err(|error| {
            format!("failed to capture Herdr command stderr: {error}")
        })?))
        .spawn()
        .map_err(|error| format!("failed to run {program}: {error}"))?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        let oversized = [&stdout, &stderr].iter().any(|file| {
            file.metadata()
                .map(|meta| meta.len() > max_output_bytes as u64)
                .unwrap_or(true)
        });
        if cancelled.load(Ordering::Relaxed) || oversized {
            stop_process_group(&mut child);
            return Err(if oversized {
                "Herdr read exceeded its output limit"
            } else {
                "Herdr read cancelled"
            }
            .to_owned());
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("failed to wait for {program}: {error}"))?
        {
            break status;
        }
        if Instant::now() >= deadline {
            stop_process_group(&mut child);
            return Err(format!(
                "Herdr command timed out after {} ms",
                timeout.as_millis()
            ));
        }
        thread::sleep(Duration::from_millis(5));
    };
    if [&stdout, &stderr].iter().any(|file| {
        file.metadata()
            .map(|meta| meta.len() > max_output_bytes as u64)
            .unwrap_or(true)
    }) {
        return Err("Herdr read exceeded its output limit".to_owned());
    }
    let stdout = read_output(stdout, "stdout", max_output_bytes)?;
    let stderr = read_output(stderr, "stderr", max_output_bytes)?;
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn output_file(stream: &str) -> Result<fs::File, String> {
    for _ in 0..16 {
        let path = env::temp_dir().join(format!(
            ".namedrop-herdr-{stream}-{}-{}",
            std::process::id(),
            NEXT_OUTPUT_FILE.fetch_add(1, Ordering::Relaxed)
        ));
        match fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => {
                fs::remove_file(&path)
                    .map_err(|error| format!("failed to secure Herdr command output: {error}"))?;
                return Ok(file);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!("failed to capture Herdr command output: {error}"));
            }
        }
    }
    Err("failed to reserve Herdr command output".to_owned())
}

fn read_output(
    mut file: fs::File,
    stream: &str,
    max_output_bytes: usize,
) -> Result<Vec<u8>, String> {
    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("failed to read Herdr command {stream}: {error}"))?;
    let mut bytes = Vec::new();
    file.take(max_output_bytes.saturating_add(1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("failed to read Herdr command {stream}: {error}"))?;
    if bytes.len() > max_output_bytes {
        return Err("Herdr read exceeded its output limit".to_owned());
    }
    Ok(bytes)
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must follow the Unix epoch")
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }
}

pub struct ProcessIds {
    sequence: AtomicU64,
}

impl Default for ProcessIds {
    fn default() -> Self {
        Self {
            sequence: AtomicU64::new(1),
        }
    }
}

impl IdGenerator for ProcessIds {
    fn next_id(&self, kind: &str) -> String {
        let unix_nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must follow the Unix epoch")
            .as_nanos();
        format!(
            "{kind}-{unix_nanos}-{}-{}",
            std::process::id(),
            self.sequence.fetch_add(1, Ordering::Relaxed)
        )
    }
}

/// Kills `child` and its whole process group. Each child runs in its own
/// group, so cancellation also stops descendants that inherited the private
/// output descriptors. The signal goes through the syscall because procps
/// `/bin/kill` parses `-<pid>` as an option and signals the wrong group.
pub(crate) fn stop_process_group(child: &mut Child) {
    if let Some(pid) = libc::pid_t::try_from(child.id())
        .ok()
        .filter(|pid| *pid > 1)
    {
        // SAFETY: kill(2) only takes a pid and a signal.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stopping_a_child_kills_its_whole_process_group_and_nothing_else() {
        let pid_file = env::temp_dir().join(format!(
            ".namedrop-process-group-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_file(&pid_file);
        let mut sentinel = Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let mut child = Command::new("sh")
            .args([
                "-c",
                &format!("sleep 30 & echo $! > '{}'; wait", pid_file.display()),
            ])
            .process_group(0)
            .spawn()
            .unwrap();
        let grandchild = wait_until(Duration::from_secs(5), || {
            fs::read_to_string(&pid_file)
                .ok()
                .and_then(|text| text.trim().parse::<u32>().ok())
        })
        .expect("the child never reported its grandchild");
        let _ = fs::remove_file(&pid_file);

        stop_process_group(&mut child);

        assert!(
            wait_until(Duration::from_secs(5), || (!is_running(grandchild))
                .then_some(()))
            .is_some(),
            "grandchild {grandchild} survived stopping the child's process group"
        );
        assert!(
            sentinel.try_wait().unwrap().is_none(),
            "a process outside the child's group was killed"
        );
        let _ = sentinel.kill();
        let _ = sentinel.wait();
    }

    fn wait_until<T>(limit: Duration, mut probe: impl FnMut() -> Option<T>) -> Option<T> {
        let deadline = Instant::now() + limit;
        loop {
            if let Some(value) = probe() {
                return Some(value);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// A reaped or zombie process no longer runs, which is what `kill -KILL`
    /// must achieve whether or not an init has collected it yet.
    fn is_running(pid: u32) -> bool {
        let state = Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .unwrap()
            .stdout;
        let state = String::from_utf8_lossy(&state);
        let state = state.trim();
        !state.is_empty() && !state.starts_with('Z')
    }
}
