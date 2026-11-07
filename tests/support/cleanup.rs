use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

pub fn remove_fixture(root: &Path) -> io::Result<()> {
    copy_retry_records(root);
    if holds_status_queue(root) {
        stop_status_workers(root);
    }
    fs::remove_dir_all(root)
}

/// The binary starts `hook agent-status-worker` detached, after every send,
/// reply and complete among others, and it can still be writing into the
/// fixture after the command that started it has exited. Its work is moot once
/// the test is over, and one blocked on a fixture's fake Herdr would hold the
/// fixture for its whole Herdr timeout, so it is killed with its process group.
fn stop_status_workers(root: &Path) {
    loop {
        let workers = status_workers(root);
        if workers.is_empty() {
            return;
        }
        for pid in workers {
            // A test can start the worker itself, outside its own group.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
                libc::kill(pid, libc::SIGKILL);
            }
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
fn status_workers(root: &Path) -> Vec<i32> {
    let mut state = b"HERDR_PLUGIN_STATE_DIR=".to_vec();
    state.extend_from_slice(root.as_os_str().as_encoded_bytes());
    let Ok(processes) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    processes
        .flatten()
        .filter_map(|entry| {
            let pid = entry.file_name().to_str()?.parse().ok()?;
            let arguments = fs::read(entry.path().join("cmdline")).ok()?;
            if !arguments
                .split(|byte| *byte == 0)
                .any(|argument| argument == b"agent-status-worker")
            {
                return None;
            }
            let environment = fs::read(entry.path().join("environ")).ok()?;
            environment
                .split(|byte| *byte == 0)
                .any(|variable| {
                    variable
                        .strip_prefix(state.as_slice())
                        .is_some_and(|rest| rest.is_empty() || rest.starts_with(b"/"))
                })
                .then_some(pid)
        })
        .collect()
}

#[cfg(not(target_os = "linux"))]
fn status_workers(_root: &Path) -> Vec<i32> {
    Vec::new()
}

/// The binary queues a worker's markers here before it starts the worker.
fn holds_status_queue(root: &Path) -> bool {
    let Ok(entries) = fs::read_dir(root) else {
        return false;
    };
    entries.flatten().any(|entry| {
        entry.file_type().is_ok_and(|kind| kind.is_dir())
            && (entry.file_name() == "agent-status-reconciliation"
                || holds_status_queue(&entry.path()))
    })
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
