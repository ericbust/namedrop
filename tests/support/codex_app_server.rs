// Each test crate uses a different part of this support module.
#![allow(dead_code)]

use std::{
    fs,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

pub const SUMMARY: &str = "Codex runs its sessions through its shared app-server, so Herdr cannot tell which pane each Codex session is in.";
pub const DISABLE: &str = "codex features disable daemon_auto_start";
pub const STOP: &str = "codex app-server daemon stop";
pub const PER_SESSION: &str = "codex --no-daemon";
pub const ENABLE: &str = "codex features enable daemon_auto_start";
pub const COMMANDS: [&str; 4] = [DISABLE, STOP, PER_SESSION, ENABLE];

/// A process group the test owns and kills on drop.
pub struct Process(Child);

impl Process {
    /// Returns once the process shows the argv[0] it was given: a vfork-based
    /// spawn can return while exec is still replacing the child's image.
    pub fn spawn(command: &mut Command, argv0: &str) -> Self {
        let process = Self(
            command
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let shown = Command::new("ps")
                .args(["-o", "args=", "-p", &process.pid().to_string()])
                .output()
                .unwrap()
                .stdout;
            if String::from_utf8_lossy(&shown).starts_with(argv0) {
                return process;
            }
            assert!(Instant::now() < deadline, "{argv0} never started");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    pub fn pid(&self) -> u32 {
        self.0.id()
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", self.0.id())])
            .status();
        let _ = self.0.wait();
    }
}

/// The path a running Codex shared app-server shows as argv[0].
pub fn server_program(codex_home: &Path) -> PathBuf {
    codex_home
        .join("packages/app-server-daemon/releases/0.162.1-x86_64-unknown-linux-musl/bin/codex")
}

/// Starts a stand-in server for `codex_home` and records it in the pid file
/// Codex writes, so Namedrop sees a running server.
pub fn start_server(codex_home: &Path) -> Process {
    let program = server_program(codex_home);
    let server = Process::spawn(
        Command::new("sleep").arg0(&program).arg("60"),
        &program.display().to_string(),
    );
    let pid_file = pid_file(codex_home);
    fs::create_dir_all(pid_file.parent().unwrap()).unwrap();
    fs::write(
        &pid_file,
        format!(
            "{{\"pid\":{},\"processStartTime\":\"Fri Oct 23 07:53:57 2026\"}}",
            server.pid()
        ),
    )
    .unwrap();
    server
}

pub fn pid_file(codex_home: &Path) -> PathBuf {
    codex_home.join("app-server-daemon/daemon.pid")
}

/// A stand-in for a pane's `codex --no-daemon`.
pub fn in_process_codex() -> Process {
    Process::spawn(
        Command::new("/bin/sh").args(["-c", "sleep 60; :", "sh", "--no-daemon"]),
        "/bin/sh",
    )
}

/// Every file and directory under `root` with its bytes, to prove a read-only
/// check left them alone.
pub fn snapshot(root: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    let mut entries = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(&path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                entries.push((path.clone(), None));
                pending.push(path);
            } else {
                entries.push((path.clone(), Some(fs::read(&path).unwrap())));
            }
        }
    }
    entries.sort();
    entries
}
