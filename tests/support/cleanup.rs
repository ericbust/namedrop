use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

pub fn remove_fixture(root: &Path) -> io::Result<()> {
    copy_retry_records(root);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        stop_background_writers(root, deadline);
        match fs::remove_dir_all(root) {
            Err(error)
                if error.kind() == io::ErrorKind::DirectoryNotEmpty
                    && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(20));
            }
            removed => return removed,
        }
    }
}

/// The binary's detached modes that write into a state directory after the
/// command that started them has exited: the mailbox worker after every send,
/// reply and complete, the deferred worker and each command it runs, and the
/// reminder clock. Their work is moot once the test is over, and one blocked on
/// a fixture's fake Herdr would hold the fixture for its whole Herdr timeout, so
/// each is killed and then awaited.
#[cfg(target_os = "linux")]
const BACKGROUND_MODES: [&[u8]; 4] = [
    b"agent-status-worker",
    b"--deferred-worker",
    b"--deferred-execute",
    b"reminder-clock",
];

fn stop_background_writers(root: &Path, deadline: Instant) {
    loop {
        let writers = background_writers(root);
        if writers.is_empty() || Instant::now() >= deadline {
            return;
        }
        for writer in &writers {
            unsafe {
                // Only a group this writer leads is the writer's own: its
                // children, never a test runner's or another fixture's.
                if writer.leads_group {
                    libc::kill(-writer.pid, libc::SIGKILL);
                }
                libc::kill(writer.pid, libc::SIGKILL);
            }
        }
        while Instant::now() < deadline && writers.iter().any(|writer| writer.alive()) {
            thread::sleep(Duration::from_millis(5));
        }
    }
}

struct Writer {
    pid: i32,
    leads_group: bool,
}

impl Writer {
    /// A killed orphan stays a zombie until its reaper collects it, and a
    /// zombie holds no files open.
    fn alive(&self) -> bool {
        process_state(self.pid).is_some_and(|state| state.0 != 'Z')
    }
}

/// `(state, process group)` from `/proc/<pid>/stat`, or `None` once it is gone.
fn process_state(pid: i32) -> Option<(char, i32)> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let mut fields = stat.rsplit_once(')')?.1.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let group = fields.nth(1)?.parse().ok()?;
    Some((state, group))
}

#[cfg(target_os = "linux")]
fn background_writers(root: &Path) -> Vec<Writer> {
    let root = root.as_os_str().as_encoded_bytes();
    let inside = |path: &[u8]| {
        path.strip_prefix(root)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(b"/"))
    };
    let Ok(processes) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    processes
        .flatten()
        .filter_map(|entry| {
            let pid: i32 = entry.file_name().to_str()?.parse().ok()?;
            let arguments = fs::read(entry.path().join("cmdline")).ok()?;
            let arguments = arguments.split(|byte| *byte == 0).collect::<Vec<_>>();
            if !arguments
                .iter()
                .any(|argument| BACKGROUND_MODES.contains(argument))
            {
                return None;
            }
            let environment = fs::read(entry.path().join("environ")).unwrap_or_default();
            let ours = arguments.iter().any(|argument| inside(argument))
                || environment.split(|byte| *byte == 0).any(|variable| {
                    variable
                        .strip_prefix(b"HERDR_PLUGIN_STATE_DIR=".as_slice())
                        .is_some_and(inside)
                });
            let (state, group) = process_state(pid)?;
            (ours && state != 'Z').then_some(Writer {
                pid,
                leads_group: group == pid,
            })
        })
        .collect()
}

#[cfg(not(target_os = "linux"))]
fn background_writers(_root: &Path) -> Vec<Writer> {
    Vec::new()
}

fn copy_retry_records(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            copy_retry_records(&entry.path());
        } else if kind.is_file()
            && entry.file_name() == "pane-effects-retries.log"
            && let Ok(record) = fs::read(entry.path())
        {
            let _ = io::stderr().lock().write_all(&record);
        }
    }
}

#[allow(dead_code)]
pub struct FixtureDirectory(pub PathBuf);

impl Drop for FixtureDirectory {
    fn drop(&mut self) {
        if let Err(error) = remove_fixture(&self.0)
            && error.kind() != io::ErrorKind::NotFound
        {
            eprintln!("remove fixture {}: {error}", self.0.display());
        }
    }
}
