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

#[derive(Default)]
pub(crate) struct CatchUp(Option<Vec<crate::StateNamespace>>);

impl CatchUp {
    #[cfg(test)]
    pub(crate) fn completed() -> Self {
        Self(Some(Vec::new()))
    }
}

#[derive(Default)]
struct Pending {
    count: u64,
    next_due_at_ms: Option<u64>,
    socket: Option<String>,
    due: Vec<crate::StateNamespace>,
    catch_up: Vec<crate::StateNamespace>,
}

fn pending(state: &Path, now: u64, startup: bool) -> Result<Pending, String> {
    let database = state.join("namedrop.sqlite3");
    if !database.is_file() {
        return Ok(Pending::default());
    }
    let connection = Connection::open_with_flags(
        database,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| e.to_string())?;
    connection
        .busy_timeout(Duration::from_millis(50))
        .map_err(|e| e.to_string())?;
    let summary = connection.query_row(
        "SELECT count(*),min(CASE WHEN state='scheduled' THEN due_at_ms END),min(session_socket_id) FROM reminders INDEXED BY reminders_pending WHERE state IN ('scheduled','awaiting_delivery')",
        [], |r| Ok(Pending { count:u64::from(r.get::<_,u32>(0)?), next_due_at_ms:r.get::<_,Option<i64>>(1)?.map(|n|n as u64), socket:r.get(2)?, ..Pending::default() })
    );
    let mut plan = match summary {
        Ok(plan) => plan,
        Err(rusqlite::Error::SqliteFailure(_, Some(message)))
            if message.contains("no such table") || message.contains("no such index") =>
        {
            return Ok(Pending::default());
        }
        Err(error) => return Err(error.to_string()),
    };
    if plan.next_due_at_ms.is_some_and(|at| at <= now) {
        let mut statement = connection.prepare("SELECT DISTINCT session_socket_id,worktree_id FROM reminders WHERE state='scheduled' AND due_at_ms<=?1").map_err(|e|e.to_string())?;
        plan.due = statement
            .query_map([now as i64], |r| {
                Ok(crate::StateNamespace::for_space(
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
    }
    if startup {
        let mut statement = connection.prepare("SELECT DISTINCT session_socket_id,worktree_id FROM reminders WHERE state='awaiting_delivery'").map_err(|e|e.to_string())?;
        plan.catch_up = statement
            .query_map([], |r| {
                Ok(crate::StateNamespace::for_space(
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                ))
            })
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
    }
    Ok(plan)
}

fn wait_ms(plan: &Pending, now: u64) -> u64 {
    plan.next_due_at_ms
        .map_or(30_000, |at| at.saturating_sub(now).clamp(100, 30_000))
}

pub fn pending_count(state: &Path) -> u64 {
    pending(state, 0, false).map_or(0, |p| p.count)
}

/// Whether a clock holds the lock. The probe asks for it shared, which only a
/// clock's exclusive hold refuses, so a clock that starts during a probe can
/// tell the probe from another clock.
pub fn clock_running(state: &Path) -> bool {
    let Ok(file) = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(state.join("reminder-clock.lock"))
    else {
        return false;
    };
    file.try_lock_shared().is_err()
}

/// Takes the clock's lock, or says another clock has it. Only a clock holds
/// it exclusively. A probe holds it shared for an instant, and a clock sent
/// away by one would leave pending reminders with no clock until the next
/// command, so this waits the probe out.
fn claim_clock(lock: &fs::File) -> bool {
    loop {
        if lock.try_lock().is_ok() {
            return true;
        }
        if lock.try_lock_shared().is_err() {
            return false;
        }
        let _ = lock.unlock();
        thread::sleep(Duration::from_millis(1));
    }
}

pub fn ensure_clock(state: &Path, socket: Option<&str>) -> Result<(), String> {
    let pending = pending(state, 0, false);
    if pending.as_ref().is_ok_and(|pending| pending.count == 0) || clock_running(state) {
        return Ok(());
    }
    let persisted_socket = match pending {
        Ok(pending) => pending.socket,
        Err(error) if socket.is_none() => return Err(error),
        Err(_) => None,
    };
    let socket = socket
        .or(persisted_socket.as_deref())
        .ok_or("pending reminders have no Herdr socket")?;
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
    ensure_clock(state, Some(socket))
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
    if !claim_clock(&lock) {
        return Ok(());
    }
    let mut catch_up = CatchUp::default();
    loop {
        let _ = fs::remove_file(state.join("reminder-clock.wake"));
        let plan = match pending(state, SystemClock.now_unix_ms(), catch_up.0.is_none()) {
            Ok(plan) => plan,
            Err(error) if transient(&error) => {
                thread::sleep(Duration::from_millis(250));
                continue;
            }
            Err(error) => return Err(error),
        };
        if plan.count == 0 {
            lock.unlock().map_err(|e| e.to_string())?;
            return ensure_clock(state, Some(socket));
        }
        let selected = selected_executable(state)?;
        let changed = fs::canonicalize(&selected).ok()
            != std::env::current_exe().and_then(fs::canonicalize).ok();
        if changed
            || !plan.due.is_empty()
            || !plan.catch_up.is_empty()
            || catch_up
                .0
                .as_ref()
                .is_some_and(|namespaces| !namespaces.is_empty())
        {
            let gate = crate::runtime_command_guard(state, None)?;
            let selected = selected_executable(state)?;
            if fs::canonicalize(&selected).ok()
                != std::env::current_exe().and_then(fs::canonicalize).ok()
            {
                drop(gate);
                lock.unlock().map_err(|e| e.to_string())?;
                let error = Command::new(selected)
                    .arg("reminder-clock")
                    .arg(state)
                    .arg(socket)
                    .exec();
                return Err(error.to_string());
            }
            match cycle_with(
                state,
                &SystemClock,
                &ProcessIds::default(),
                |ns| {
                    ProcessHerdr::default()
                        .with_socket(Some(ns.session_socket_id.clone()))
                        .keyed(&ns.session_socket_id)
                },
                &mut catch_up,
            ) {
                Ok(_) => {}
                Err(error) if transient(&error) => {
                    drop(gate);
                    thread::sleep(Duration::from_millis(250));
                    continue;
                }
                Err(error) => return Err(error),
            }
        } else {
            catch_up.0 = Some(Vec::new());
        }
        let plan = match pending(state, SystemClock.now_unix_ms(), false) {
            Ok(plan) => plan,
            Err(error) if transient(&error) => {
                thread::sleep(Duration::from_millis(250));
                continue;
            }
            Err(error) => return Err(error),
        };
        if plan.count == 0 {
            continue;
        }
        let wait_ms = wait_ms(&plan, SystemClock.now_unix_ms());
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

pub(crate) fn cycle_with<E, C, I>(
    state: &Path,
    clock: &C,
    ids: &I,
    mut herdr_for: impl FnMut(&crate::StateNamespace) -> E,
    catch_up: &mut CatchUp,
) -> Result<bool, String>
where
    E: crate::mux::Mux,
    C: Clock,
    I: crate::IdGenerator,
{
    let initializing = catch_up.0.is_none();
    let plan = pending(state, clock.now_unix_ms(), initializing)?;
    let remaining = catch_up.0.get_or_insert(plan.catch_up);
    let mut drains = remaining.clone();
    for ns in plan.due {
        let herdr = herdr_for(&ns);
        let live = match crate::lifecycle::discover_reminder_workspace(state, &ns, &herdr) {
            Ok(live) => live,
            Err(error)
                if crate::lifecycle::herdr_error_code(&error).as_deref()
                    == Some("workspace_not_found") =>
            {
                drains.retain(|candidate| candidate != &ns);
                remaining.retain(|candidate| candidate != &ns);
                continue;
            }
            Err(_) => continue,
        };
        let now = clock.now_unix_ms();
        let enqueued = StateStore::open(state)
            .map_err(|e| e.to_string())?
            .transaction(|tx| {
                let mut enqueued = false;
                for reminder in tx.due_reminders(&ns, now)? {
                    enqueued |= tx.enqueue_reminder(&ns, &reminder.reminder_id, &live, now)?;
                }
                Ok(enqueued)
            })
            .map_err(|e| e.to_string())?;
        if enqueued && !drains.contains(&ns) {
            drains.push(ns);
        }
    }
    for ns in drains {
        let herdr = herdr_for(&ns);
        let runtime = LifecycleRuntime {
            state_directory: state,
            home_directory: None,
            agent_config: crate::AgentConfigDirectories::default(),
            cwd: state,
            workspace_id: Some(ns.space_id.as_str()),
            current_pane_id: None,
            session_socket_id: &ns.session_socket_id,
            mux: &herdr,
            clock,
            ids,
        };
        let result = crate::drain_mailboxes(&runtime);
        #[cfg(test)]
        crate::work_counters::drain_result(&result);
        if result.as_ref().err().is_none_or(|error| {
            crate::lifecycle::herdr_error_code(error).as_deref() == Some("workspace_not_found")
        }) {
            remaining.retain(|candidate| candidate != &ns);
        } else if initializing && !remaining.contains(&ns) {
            remaining.push(ns);
        }
    }
    Ok(remaining.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IdGenerator;
    #[test]
    fn reminder_idle_wait_is_bounded_by_deadline_and_rescan() {
        let mut plan = Pending::default();
        assert_eq!(wait_ms(&plan, 0), 30_000);
        plan.next_due_at_ms = Some(10_000);
        assert_eq!(wait_ms(&plan, 500), 9_500);
        assert_eq!(wait_ms(&plan, 20_000), 100);
        plan.next_due_at_ms = Some(100_000);
        assert_eq!(wait_ms(&plan, 0), 30_000);
    }

    #[test]
    fn a_clock_outlasts_a_probe_and_yields_only_to_another_clock() {
        let root = std::env::temp_dir().join(ProcessIds::default().next_id("test-reminder-claim"));
        fs::create_dir(&root).unwrap();
        let path = root.join("reminder-clock.lock");
        let open = || {
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
                .unwrap()
        };
        let (probe, clock, later) = (open(), open(), open());

        probe.try_lock_shared().unwrap();
        assert!(!clock_running(&root), "a probe looked like a clock");
        let (left_early, claimed) = thread::scope(|scope| {
            let claim = scope.spawn(|| claim_clock(&clock));
            let patience = std::time::Instant::now() + Duration::from_millis(100);
            while !claim.is_finished() && std::time::Instant::now() < patience {
                thread::sleep(Duration::from_millis(5));
            }
            let left_early = claim.is_finished();
            probe.unlock().unwrap();
            (left_early, claim.join().unwrap())
        });
        assert!(!left_early, "a probe sent the clock away");
        assert!(claimed);

        assert!(clock_running(&root));
        assert!(!claim_clock(&later), "two clocks hold the lock");
        drop((probe, clock, later));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reminder_pending_probe_and_doctor_lock_probe_do_not_create_state() {
        let root = std::env::temp_dir().join(ProcessIds::default().next_id("test-reminder-probe"));
        fs::create_dir(&root).unwrap();
        assert_eq!(pending_count(&root), 0);
        assert!(!clock_running(&root));
        ensure_clock(&root, None).unwrap();
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        let store = StateStore::open(&root).unwrap();
        drop(store);
        let connection = Connection::open(root.join("namedrop.sqlite3")).unwrap();
        connection.execute_batch("BEGIN IMMEDIATE").unwrap();
        assert_eq!(pending_count(&root), 0);
        ensure_clock(&root, None).unwrap();
        assert!(!root.join("reminder-clock.lock").exists());
        connection.execute_batch("ROLLBACK").unwrap();
        drop(connection);
        fs::remove_dir_all(root).unwrap();
    }
}
