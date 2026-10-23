use crate::lifecycle::locate_agent_session_pane;
use crate::{ActorKind, Clock, DeliveryAttemptId, IdGenerator, MutationAttribution, StateStore};
use crate::{
    HerdrCommandExecutor, LifecycleRuntime, PaneType, discover_worktree,
    discover_worktree_for_workspace,
};
use serde::Deserialize;
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    thread,
};

pub(crate) fn runner_command<E, C, I>(
    runtime: &LifecycleRuntime<'_, E, C, I>,
    workspace: &str,
    pane: &str,
    attempt: &DeliveryAttemptId,
) -> Result<String, String> {
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\"'\"'"));
    Ok([
        crate::collaboration_executable()?,
        "--plugin-state".to_owned(),
        runtime.state_directory.display().to_string(),
        "--herdr-socket".to_owned(),
        runtime.session_socket_id.to_owned(),
        "--herdr-workspace".to_owned(),
        workspace.to_owned(),
        "--herdr-pane".to_owned(),
        pane.to_owned(),
        "terminal-execute".to_owned(),
        attempt.as_str().to_owned(),
    ]
    .iter()
    .map(|s| quote(s))
    .collect::<Vec<_>>()
    .join(" "))
}

/// Runs inside the target terminal. The atomic claim rejects replay and binds
/// execution to the physical terminal, not a potentially recycled pane label.
pub fn execute_terminal<E: HerdrCommandExecutor, C: Clock, I: IdGenerator>(
    attempt: &str,
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Result<i32, String> {
    let discovered = match runtime.workspace_id {
        Some(id) => discover_worktree_for_workspace(runtime.herdr, id)?,
        None => discover_worktree(runtime.herdr, runtime.cwd)?,
    };
    let ns = discovered.namespace(runtime.session_socket_id);
    let pane = discovered
        .panes
        .iter()
        .find(|p| Some(p.pane_id.as_str()) == runtime.current_pane_id)
        .ok_or("terminal runner requires its current Herdr pane")?;
    if pane.agent.is_some() || pane.agent_session_id.is_some() {
        return Err("terminal pane now contains an agent; command was not executed".to_owned());
    }
    if !process_info(runtime.herdr, &pane.pane_id).is_some_and(|p| {
        p.foreground_processes
            .iter()
            .any(|p| p.pid == std::process::id())
    }) {
        return Err(
            "terminal runner must be running in the target pane's foreground process group"
                .to_owned(),
        );
    }
    let attempt = DeliveryAttemptId::new(attempt);
    let attribution = || {
        MutationAttribution::new(
            ActorKind::Plugin,
            "namedrop-terminal",
            runtime.clock.now_unix_ms(),
        )
    };
    let mut store = StateStore::open(runtime.state_directory).map_err(|e| e.to_string())?;
    let command = store
        .transaction(|tx| tx.claim_terminal_execution(&ns, &attempt, pane, &attribution()))
        .map_err(|e| e.to_string())?;
    println!("$ {command}");
    let (code, output) = match run_shell(&command) {
        Ok(result) => result,
        Err(error) => (127, error),
    };
    let output = crate::context::redact_disclosure_text(&output);
    store
        .transaction(|tx| tx.record_terminal_result(&ns, &attempt, code, &output, &attribution()))
        .map_err(|e| e.to_string())?;
    store
        .transaction(|tx| {
            tx.finish_terminal_execution(&ns, &attempt, code, &output, &attribution())
        })
        .map_err(|e| e.to_string())?;
    println!("[Namedrop: exit {code}]");
    Ok(code)
}

fn run_shell(command: &str) -> Result<(i32, String), String> {
    let mut child = Command::new("/bin/sh")
        .args(["-c", command])
        .stdin(Stdio::inherit())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let out = thread::spawn(move || copy_output(stdout, std::io::stdout()));
    let err = thread::spawn(move || copy_output(stderr, std::io::stderr()));
    let status = child.wait().map_err(|e| e.to_string())?;
    let stdout = out.join().map_err(|_| "stdout reader failed")??;
    let stderr = err.join().map_err(|_| "stderr reader failed")??;
    use std::os::unix::process::ExitStatusExt;
    Ok((
        status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)),
        format!("stdout:\n{stdout}\nstderr:\n{stderr}"),
    ))
}

fn copy_output(mut reader: impl Read, mut writer: impl Write) -> Result<String, String> {
    const LIMIT: usize = 32 * 1024;
    let mut captured = Vec::new();
    let mut buffer = [0; 4096];
    let mut truncated = false;
    loop {
        let n = reader.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        let _ = writer.write_all(&buffer[..n]);
        let _ = writer.flush();
        let keep = n.min(LIMIT - captured.len());
        captured.extend_from_slice(&buffer[..keep]);
        truncated |= keep < n;
    }
    let mut text = String::from_utf8_lossy(&captured).into_owned();
    if truncated {
        text.push_str("\n[output truncated at 32 KiB; full output was streamed to the terminal]");
    }
    Ok(text)
}

#[derive(Deserialize)]
struct ProcessInfo {
    shell_pid: u64,
    foreground_process_group_id: u64,
    #[serde(default)]
    foreground_processes: Vec<Process>,
}
#[derive(Deserialize)]
struct Process {
    pid: u32,
}

pub(crate) fn shell_is_available(herdr: &impl HerdrCommandExecutor, pane_id: &str) -> bool {
    process_info(herdr, pane_id)
        .is_some_and(|p| p.shell_pid > 0 && p.shell_pid == p.foreground_process_group_id)
}

fn process_info(herdr: &impl HerdrCommandExecutor, pane_id: &str) -> Option<ProcessInfo> {
    herdr
        .execute(&["pane", "process-info", "--pane", pane_id])
        .ok()
        .and_then(|out| parse_process_info(&out.stdout))
}

fn parse_process_info(stdout: &str) -> Option<ProcessInfo> {
    serde_json::from_str::<serde_json::Value>(stdout)
        .ok()
        .and_then(|v| {
            serde_json::from_value::<ProcessInfo>(v["result"]["process_info"].clone()).ok()
        })
}

/// A pane ID inherited from the environment only identifies this process when
/// the process runs inside that pane, under its shell, or its agent session is
/// the one Herdr says runs in that pane (Claude Code's Bash can sit outside the
/// shell's process tree). A daemon keeps the variables of the pane it was first
/// started from, so the inherited value can name a pane the caller left long
/// ago. Acting as that pane is a silent identity error, so the check refuses
/// instead of falling back. When Herdr does not answer, nothing was learned
/// about the pane, so its own error is the reason given.
pub fn verify_inherited_pane(
    herdr: &impl HerdrCommandExecutor,
    pane_id: &str,
    agent_session_ids: &[&str],
) -> Result<(), String> {
    let remedy = "Pass --herdr-pane <pane-id> to say which pane this command acts for.";
    let output = herdr
        .execute(&["pane", "process-info", "--pane", pane_id])
        .map_err(|error| {
            format!("could not verify the inherited HERDR_PANE_ID={pane_id}: {error} {remedy}")
        })?;
    let shell_pid = parse_process_info(&output.stdout)
        .map(|p| p.shell_pid)
        .filter(|pid| *pid > 0)
        .ok_or_else(|| {
            format!(
                "HERDR_PANE_ID={pane_id} was inherited from the environment, but Herdr reports \
                 no shell for pane {pane_id}, so the pane could not be verified. {remedy}"
            )
        })?;
    let caller = std::process::id();
    if ancestor_pids(caller).any(|pid| u64::from(pid) == shell_pid) {
        return Ok(());
    }
    if agent_session_ids.iter().any(|session| {
        locate_agent_session_pane(herdr, session).is_some_and(|(_, pane)| pane == pane_id)
    }) {
        return Ok(());
    }
    let sessions = if agent_session_ids.is_empty() {
        "There is no CODEX_SESSION_ID or CLAUDE_CODE_SESSION_ID in the environment to vouch \
         for it."
            .to_owned()
    } else {
        format!(
            "None of the {} agent session ID(s) in the environment runs in pane {pane_id}.",
            agent_session_ids.len()
        )
    };
    Err(format!(
        "HERDR_PANE_ID={pane_id} was inherited from the environment, but this process (pid \
         {caller}) is not running inside pane {pane_id}: the pane's shell (pid {shell_pid}) is \
         not one of its ancestors. {sessions} A background daemon keeps the pane variables it \
         was started with. {remedy}"
    ))
}

/// The ancestors of `pid`, nearest first, at most 1024 of them.
pub(crate) fn ancestor_pids(pid: u32) -> impl Iterator<Item = u32> {
    std::iter::successors(parent_pid(pid), |pid| parent_pid(*pid)).take(1024)
}

/// The pids Herdr reports in a pane's foreground process group, or none when
/// Herdr does not answer.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "its surfaces arrive in the following commits")
)]
pub(crate) fn foreground_process_ids(herdr: &impl HerdrCommandExecutor, pane_id: &str) -> Vec<u32> {
    process_info(herdr, pane_id)
        .map(|info| info.foreground_processes.iter().map(|p| p.pid).collect())
        .unwrap_or_default()
}

/// A process's argv, or `None` when the process is gone or unreadable.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "its surfaces arrive in the following commits")
)]
#[cfg(target_os = "linux")]
pub(crate) fn process_arguments(pid: u32) -> Option<Vec<String>> {
    let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let bytes = bytes.strip_suffix(&[0]).unwrap_or(&bytes);
    (!bytes.is_empty()).then(|| split_arguments(bytes).collect())
}

/// A process's argv, or `None` when the process is gone or unreadable.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "its surfaces arrive in the following commits")
)]
#[cfg(target_os = "macos")]
pub(crate) fn process_arguments(pid: u32) -> Option<Vec<String>> {
    let pid = libc::c_int::try_from(pid).ok()?;
    let mut argument_max: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
    // SAFETY: sysctl writes at most `size` bytes, the size of `argument_max`.
    let read = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            2,
            (&raw mut argument_max).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if read != 0 {
        return None;
    }
    let mut buffer = vec![0_u8; usize::try_from(argument_max).ok()?];
    let mut size = buffer.len();
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    // SAFETY: sysctl writes at most `size` bytes, the length of `buffer`, and
    // sets `size` to the number it wrote.
    let read = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buffer.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if read != 0 {
        return None;
    }
    buffer.truncate(size);
    // KERN_PROCARGS2 is argc, the executable path, NUL padding, then argv.
    let (argc, rest) = buffer.split_first_chunk::<4>()?;
    let argc = usize::try_from(libc::c_int::from_ne_bytes(*argc)).ok()?;
    let rest = &rest[rest.iter().position(|b| *b == 0)?..];
    let rest = &rest[rest.iter().position(|b| *b != 0)?..];
    let arguments = split_arguments(rest).take(argc).collect::<Vec<_>>();
    (arguments.len() == argc).then_some(arguments)
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "its surfaces arrive in the following commits")
)]
fn split_arguments(bytes: &[u8]) -> impl Iterator<Item = String> {
    bytes
        .split(|b| *b == 0)
        .map(|argument| String::from_utf8_lossy(argument).into_owned())
}

/// The path of the executable a process runs, or `None` when unreadable.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "its surfaces arrive in the following commits")
)]
#[cfg(target_os = "linux")]
pub(crate) fn process_executable(pid: u32) -> Option<std::path::PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

/// The path of the executable a process runs, or `None` when unreadable.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "its surfaces arrive in the following commits")
)]
#[cfg(target_os = "macos")]
pub(crate) fn process_executable(pid: u32) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let pid = libc::c_int::try_from(pid).ok()?;
    let mut buffer = vec![0_u8; usize::try_from(libc::PROC_PIDPATHINFO_MAXSIZE).ok()?];
    // SAFETY: proc_pidpath writes at most the given size into `buffer` and
    // returns the length of the path it wrote.
    let written = unsafe {
        libc::proc_pidpath(
            pid,
            buffer.as_mut_ptr().cast(),
            u32::try_from(buffer.len()).ok()?,
        )
    };
    let written = usize::try_from(written).ok().filter(|n| *n > 0)?;
    Some(std::ffi::OsStr::from_bytes(&buffer[..written]).into())
}

#[cfg(target_os = "linux")]
fn parent_pid(pid: u32) -> Option<u32> {
    // `/proc/<pid>/stat` is `pid (comm) state ppid ...`; comm may hold spaces
    // and parentheses, so the fields start after the last `)`.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let (_, after_comm) = stat.rsplit_once(')')?;
    let parent = after_comm.split_whitespace().nth(1)?.parse::<u32>().ok()?;
    (parent != 0).then_some(parent)
}

#[cfg(target_os = "macos")]
fn parent_pid(pid: u32) -> Option<u32> {
    let pid = libc::pid_t::try_from(pid).ok()?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: proc_pidinfo writes at most `size` bytes into `info`, and only a
    // return value equal to `size` means the whole struct was filled.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        return None;
    }
    // SAFETY: the call above filled every byte of the struct.
    let info = unsafe { info.assume_init() };
    (info.pbi_ppid != 0).then_some(info.pbi_ppid)
}

/// Detection never infers a terminal merely from a missing native session ID.
pub fn detect_pane_type<E: HerdrCommandExecutor, C, I>(
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Result<Option<PaneType>, String> {
    let discovered = match runtime.workspace_id {
        Some(id) => discover_worktree_for_workspace(runtime.herdr, id)?,
        None => discover_worktree(runtime.herdr, runtime.cwd)?,
    };
    let pane = discovered
        .panes
        .iter()
        .find(|p| Some(p.pane_id.as_str()) == runtime.current_pane_id)
        .ok_or_else(|| "name must run from the Herdr pane being named".to_owned())?;
    if pane.agent.is_some() || pane.agent_session_id.is_some() {
        return Ok(Some(PaneType::Agent));
    }
    Ok(process_info(runtime.herdr, &pane.pane_id)
        .is_some_and(|p| {
            p.shell_pid > 0
                && (p.shell_pid == p.foreground_process_group_id
                    || p.foreground_processes
                        .iter()
                        .any(|p| p.pid == std::process::id()))
        })
        .then_some(PaneType::Terminal))
}

/// Both naming surfaces retain a saved type before considering live detection.
/// Opening/cancelling a prompt must not create or migrate persisted state.
pub fn default_pane_type<E: HerdrCommandExecutor, C, I>(
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Result<Option<PaneType>, String> {
    let discovered = match runtime.workspace_id {
        Some(id) => discover_worktree_for_workspace(runtime.herdr, id)?,
        None => discover_worktree(runtime.herdr, runtime.cwd)?,
    };
    if let Some(mut store) =
        StateStore::open_read_only(runtime.state_directory).map_err(|e| e.to_string())?
        && let Some(kind) = store
            .transaction(|tx| {
                Ok(tx
                    .list_pane_addresses(&discovered.namespace(runtime.session_socket_id))?
                    .into_iter()
                    .find(|a| a.pane_id.as_deref() == runtime.current_pane_id)
                    .map(|a| a.pane_type))
            })
            .map_err(|e| e.to_string())?
    {
        return Ok(Some(kind));
    }
    detect_pane_type(runtime)
}
