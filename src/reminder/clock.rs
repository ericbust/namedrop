use crate::{Clock, LifecycleRuntime, ProcessHerdr, ProcessIds, StateStore, SystemClock};
use rusqlite::{Connection, OpenFlags};
use std::{
    fs,
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::Duration,
};

fn selected_executable(state: &Path) -> Result<std::path::PathBuf, String> {
    let launcher = state.join("runtime/current/namedrop");
    if launcher.is_file() {
        Ok(launcher)
    } else {
        crate::runtime_executable()
    }
}

fn pending(state: &Path) -> Result<(u64, Option<u64>, bool), String> {
    let database = state.join("namedrop.sqlite3");
    if !database.is_file() {
        return Ok((0, None, false));
    }
    let connection = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| e.to_string())?;
    connection
        .busy_timeout(Duration::from_millis(50))
        .map_err(|e| e.to_string())?;
    match connection.query_row("SELECT count(*),min(due_at_ms),coalesce(max(state='awaiting_delivery'),0) FROM reminders INDEXED BY reminders_pending WHERE state IN ('scheduled','awaiting_delivery')",[],|r|Ok((u64::from(r.get::<_,u32>(0)?),r.get::<_,Option<i64>>(1)?.map(|n|n as u64),r.get::<_,bool>(2)?))) {
        Ok(pending)=>Ok(pending),
        Err(rusqlite::Error::SqliteFailure(_,Some(message))) if message.contains("no such table") || message.contains("no such index")=>Ok((0,None,false)),
        Err(error)=>Err(error.to_string()),
    }
}

pub fn pending_count(state: &Path) -> u64 {
    pending(state).map_or(0, |p| p.0)
}

pub fn clock_running(state: &Path) -> bool {
    let Ok(file) = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(state.join("reminder-clock.lock"))
    else {
        return false;
    };
    file.try_lock().is_err()
}

pub fn ensure_clock(state: &Path, socket: &str) -> Result<(), String> {
    if pending(state).is_ok_and(|pending| pending.0 == 0) || clock_running(state) {
        return Ok(());
    }
    let executable = selected_executable(state)?;
    let mut command = Command::new(executable);
    command
        .args(["reminder-clock"])
        .arg(state)
        .arg(socket)
        .env_remove("HERDR_PANE_ID")
        .env_remove("HERDR_WORKSPACE_ID")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("CODEX_SESSION_ID")
        .env_remove("CLAUDE_SESSION_ID")
        .env_remove("CODEX_THREAD_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    command.spawn().map_err(|e| e.to_string())?;
    Ok(())
}

pub(super) fn signal_clock(state: &Path, socket: &str) -> Result<(), String> {
    fs::write(state.join("reminder-clock.wake"), []).map_err(|e| e.to_string())?;
    ensure_clock(state, socket)
}

pub fn run_clock(state: &Path, socket: &str) -> Result<(), String> {
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(state.join("reminder-clock.lock"))
        .map_err(|e| e.to_string())?;
    if lock.try_lock().is_err() {
        return Ok(());
    }
    loop {
        let _ = fs::remove_file(state.join("reminder-clock.wake"));
        let (count, _, _) = match pending(state) {
            Ok(pending) => pending,
            Err(error) if transient(&error) => {
                thread::sleep(Duration::from_millis(250));
                continue;
            }
            Err(error) => return Err(error),
        };
        if count == 0 {
            lock.unlock().map_err(|e| e.to_string())?;
            return ensure_clock(state, socket);
        }
        {
            let _gate = crate::runtime_command_guard(state)?;
            let selected = selected_executable(state)?;
            if fs::canonicalize(&selected).ok()
                != std::env::current_exe().and_then(fs::canonicalize).ok()
            {
                drop(_gate);
                lock.unlock().map_err(|e| e.to_string())?;
                let error = Command::new(selected)
                    .arg("reminder-clock")
                    .arg(state)
                    .arg(socket)
                    .exec();
                return Err(error.to_string());
            }
            if let Err(error) = cycle(state) {
                if !transient(&error) {
                    return Err(error);
                }
                drop(_gate);
                thread::sleep(Duration::from_millis(250));
                continue;
            }
        }
        let (remaining, next, awaiting) = match pending(state) {
            Ok(pending) => pending,
            Err(error) if transient(&error) => {
                thread::sleep(Duration::from_millis(250));
                continue;
            }
            Err(error) => return Err(error),
        };
        if remaining == 0 {
            continue;
        }
        let wait_ms = next.map_or(1000, |at| {
            at.saturating_sub(SystemClock.now_unix_ms())
                .clamp(100, if awaiting { 1000 } else { 60_000 })
        });
        let mut waited = 0;
        while waited < wait_ms && !state.join("reminder-clock.wake").exists() {
            let step = (wait_ms - waited).min(100);
            thread::sleep(Duration::from_millis(step));
            waited += step;
        }
    }
}

fn transient(error: &str) -> bool {
    error.contains("busy") || error.contains("locked")
}

fn cycle(state: &Path) -> Result<(), String> {
    let ids = ProcessIds::default();
    let mut store = StateStore::open(state).map_err(|e| e.to_string())?;
    let namespaces = store
        .transaction(|tx| tx.reminder_namespaces())
        .map_err(|e| e.to_string())?;
    for ns in namespaces {
        let socket = &ns.session_socket_id;
        let herdr = ProcessHerdr::default().with_socket(Some(socket.to_owned()));
        let live = match crate::discover_worktree_for_workspace(&herdr, ns.space_id.as_str()) {
            Ok(live) => live,
            Err(_) => continue,
        };
        let now = SystemClock.now_unix_ms();
        store
            .transaction(|tx| {
                tx.maintain_reminder_owners(&ns, &live)?;
                for reminder in tx.due_reminders(&ns, now)? {
                    tx.enqueue_reminder(&ns, &reminder.reminder_id, &live, now)?;
                }
                Ok(())
            })
            .map_err(|e| e.to_string())?;
        let runtime = LifecycleRuntime {
            state_directory: state,
            home_directory: None,
            agent_config: crate::AgentConfigDirectories::default(),
            cwd: state,
            workspace_id: Some(ns.space_id.as_str()),
            current_pane_id: None,
            session_socket_id: socket,
            herdr: &herdr,
            clock: &SystemClock,
            ids: &ids,
        };
        let _ = crate::drain_mailboxes(&runtime);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IdGenerator;
    #[test]
    fn reminder_pending_probe_and_doctor_lock_probe_do_not_create_state() {
        let root = std::env::temp_dir().join(ProcessIds::default().next_id("test-reminder-probe"));
        fs::create_dir(&root).unwrap();
        assert_eq!(pending_count(&root), 0);
        assert!(!clock_running(&root));
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        let store = StateStore::open(&root).unwrap();
        drop(store);
        let connection = Connection::open(root.join("namedrop.sqlite3")).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert_eq!(pending_count(&root), 0);
        assert!(!root.join("reminder-clock.lock").exists());
        connection.execute_batch("ROLLBACK").unwrap();
        drop(connection);
        fs::remove_dir_all(root).unwrap();
    }
}
