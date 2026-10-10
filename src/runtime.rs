use crate::{Clock, HerdrCommandExecutor, HerdrCommandOutput, IdGenerator};
use std::{
    env, fs,
    io::{Read, Seek, SeekFrom},
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

static NEXT_HERDR_OUTPUT_FILE: AtomicU64 = AtomicU64::new(1);
const DEFAULT_HERDR_TIMEOUT_MS: u64 = 10_000;
const MAX_HERDR_TIMEOUT_MS: u64 = 60_000;

pub struct ProcessHerdr {
    binary: String,
    timeout: Duration,
    socket: Option<String>,
}

impl Default for ProcessHerdr {
    fn default() -> Self {
        Self {
            binary: env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".to_owned()),
            socket: None,
            timeout: Duration::from_millis(
                env::var("NAMEDROP_HERDR_TIMEOUT_MS")
                    .ok()
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|milliseconds| *milliseconds > 0)
                    .unwrap_or(DEFAULT_HERDR_TIMEOUT_MS)
                    .min(MAX_HERDR_TIMEOUT_MS),
            ),
        }
    }
}

impl ProcessHerdr {
    pub fn with_socket(mut self, socket: Option<String>) -> Self {
        self.socket = socket;
        self
    }

    fn output_file(&self, stream: &str) -> Result<fs::File, String> {
        for _ in 0..16 {
            let path = env::temp_dir().join(format!(
                ".namedrop-herdr-{stream}-{}-{}",
                std::process::id(),
                NEXT_HERDR_OUTPUT_FILE.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(file) => {
                    fs::remove_file(&path).map_err(|error| {
                        format!("failed to secure Herdr command output: {error}")
                    })?;
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
        &self,
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

    fn output(&self, args: &[&str]) -> Result<Output, String> {
        self.output_bounded(args, self.timeout, &AtomicBool::new(false), usize::MAX)
    }

    fn output_bounded(
        &self,
        args: &[&str],
        timeout: Duration,
        cancelled: &AtomicBool,
        max_output_bytes: usize,
    ) -> Result<Output, String> {
        if cancelled.load(Ordering::Relaxed) {
            return Err("Herdr read cancelled".to_owned());
        }
        let mut command = Command::new(&self.binary);
        command.args(args).process_group(0);
        if let Some(socket) = &self.socket {
            command.env("HERDR_SOCKET_PATH", socket);
        }
        let timeout = timeout.min(self.timeout);
        let stdout = self.output_file("stdout")?;
        let stderr = self.output_file("stderr")?;
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout.try_clone().map_err(|error| {
                format!("failed to capture Herdr command stdout: {error}")
            })?))
            .stderr(Stdio::from(stderr.try_clone().map_err(|error| {
                format!("failed to capture Herdr command stderr: {error}")
            })?))
            .spawn()
            .map_err(|error| format!("failed to run {}: {error}", self.binary))?;
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
                .map_err(|error| format!("failed to wait for {}: {error}", self.binary))?
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
        let stdout = self.read_output(stdout, "stdout", max_output_bytes)?;
        let stderr = self.read_output(stderr, "stderr", max_output_bytes)?;
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }
}

impl HerdrCommandExecutor for ProcessHerdr {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let output = self.output(args)?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }
        Ok(HerdrCommandOutput {
            stdout: String::from_utf8(output.stdout).map_err(|error| error.to_string())?,
        })
    }

    fn execute_bounded(
        &self,
        args: &[&str],
        timeout: Duration,
        cancelled: &AtomicBool,
        max_output_bytes: usize,
    ) -> Result<HerdrCommandOutput, String> {
        let output = self.output_bounded(args, timeout, cancelled, max_output_bytes)?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }
        Ok(HerdrCommandOutput {
            stdout: String::from_utf8(output.stdout).map_err(|error| error.to_string())?,
        })
    }

    fn classify_prompt_failure(&self, error: &str) -> crate::PromptFailureKind {
        let code = serde_json::from_str::<serde_json::Value>(error)
            .ok()
            .and_then(|value| {
                value
                    .pointer("/error/code")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            });
        match code.as_deref() {
            // `herdr agent prompt --help` (0.8.2): "If the agent is already
            // blocked, submission is rejected with `agent_blocked` before any
            // input is sent." Like `agent_busy`, the recipient never saw the
            // prompt, so requeueing cannot duplicate an invocation.
            Some(
                "agent_not_found" | "pane_not_found" | "agent_not_ready" | "agent_busy"
                | "agent_blocked" | "conflict" | "server_busy",
            ) => crate::PromptFailureKind::TemporaryPreInvocation,
            Some(
                "invalid_request" | "invalid_args" | "invalid_input" | "unsupported"
                | "unknown_command",
            ) => crate::PromptFailureKind::DefinitiveNonRetryable,
            _ => crate::PromptFailureKind::AmbiguousInvocation,
        }
    }
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
mod bounded_read_tests {
    use super::*;

    #[test]
    fn bounded_reads_enforce_output_deadline_and_cancellation() {
        let executor = ProcessHerdr {
            binary: "/bin/sh".to_owned(),
            timeout: Duration::from_secs(2),
            socket: None,
        };
        let cancelled = AtomicBool::new(false);
        let deadline = Duration::from_millis(80);
        assert_eq!(
            executor
                .execute_bounded(&["-c", "printf ok"], deadline, &cancelled, 2)
                .unwrap()
                .stdout,
            "ok"
        );
        assert!(
            executor
                .execute_bounded(&["-c", "printf too-long"], deadline, &cancelled, 2)
                .unwrap_err()
                .contains("output limit")
        );
        let start = Instant::now();
        assert!(
            executor
                .execute_bounded(&["-c", "sleep 10"], deadline, &cancelled, 100)
                .unwrap_err()
                .contains("timed out")
        );
        assert!(start.elapsed() < Duration::from_secs(1));
        thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(30));
                cancelled.store(true, Ordering::Relaxed);
            });
            assert!(
                executor
                    .execute_bounded(&["-c", "sleep 10"], Duration::from_secs(1), &cancelled, 100)
                    .unwrap_err()
                    .contains("cancelled")
            );
        });
    }

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
