//! What this machine says about its own processes: parent, process group,
//! executable, and start time. Every check that a process is alive goes
//! through `LocalProcesses::read`, which reads a zombie as no process.

use super::Incarnation;
use std::path::Path;

/// One local read of a pid.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum LocalRead {
    Alive(LocalProc),
    /// No process has the pid, or a zombie has it.
    Gone,
    /// The process may exist, and its facts cannot be read.
    Unreadable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LocalProc {
    /// `<pid>@<start>`, which a reused pid never repeats.
    pub incarnation: Incarnation,
    pub group: u32,
}

pub(crate) trait LocalProcesses: Send + Sync {
    fn read(&self, pid: u32) -> LocalRead;
    /// Every pid the system lists in a process group, which can include
    /// zombies, or `None` when the group cannot be listed. Read members with
    /// `members`.
    fn list_group(&self, group: u32) -> Option<Vec<u32>>;
    /// The parent, read with the short call that also reads root's
    /// processes, such as the `login` under a Rex block's shell.
    fn parent(&self, pid: u32) -> Option<u32>;
    /// The executable, with symlinks resolved.
    fn executable(&self, pid: u32) -> Option<std::path::PathBuf>;
}

/// The ancestors of `pid`, its parent first, as far as they can be read.
pub(crate) fn ancestors(local: &dyn LocalProcesses, pid: u32) -> Vec<u32> {
    let mut ancestors = Vec::new();
    let mut pid = pid;
    while let Some(parent) = local.parent(pid) {
        if ancestors.len() == 64 || ancestors.contains(&parent) {
            break;
        }
        ancestors.push(parent);
        pid = parent;
    }
    ancestors
}

/// The live members of a process group: each listed pid passes through
/// `read`, and a `Gone` one, such as a zombie, is dropped.
pub(crate) fn members(local: &dyn LocalProcesses, group: u32) -> Option<Vec<u32>> {
    let listed = local.list_group(group)?;
    Some(
        listed
            .into_iter()
            .filter(|pid| local.read(*pid) != LocalRead::Gone)
            .collect(),
    )
}

/// A process is a shell by its executable's name, never by `comm`, which
/// Linux sets to a script's truncated name.
pub(crate) fn is_shell(executable: &Path) -> bool {
    executable
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            matches!(
                name,
                "sh" | "bash" | "zsh" | "dash" | "ksh" | "fish" | "busybox"
            )
        })
}

/// The processes of the machine this runs on.
pub(crate) struct ThisMachine;

impl LocalProcesses for ThisMachine {
    fn read(&self, pid: u32) -> LocalRead {
        platform::read(pid)
    }

    fn list_group(&self, group: u32) -> Option<Vec<u32>> {
        platform::group(group)
    }

    fn parent(&self, pid: u32) -> Option<u32> {
        platform::parent(pid)
    }

    fn executable(&self, pid: u32) -> Option<std::path::PathBuf> {
        platform::executable(pid)
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{LocalProc, LocalRead};
    use crate::mux::Incarnation;
    use std::mem::MaybeUninit;

    /// `PROC_PIDTBSDINFO` with `arg` 0 fails with `ESRCH` on a zombie, as on
    /// a pid with no process, and with another error on another user's
    /// process.
    pub(super) fn read(pid: u32) -> LocalRead {
        match info::<libc::proc_bsdinfo>(pid, libc::PROC_PIDTBSDINFO) {
            Some(info) => {
                let start = info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec;
                LocalRead::Alive(LocalProc {
                    incarnation: Incarnation(format!("{pid}@{start}")),
                    group: info.pbi_pgid,
                })
            }
            None if std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) => {
                LocalRead::Gone
            }
            None => LocalRead::Unreadable,
        }
    }

    pub(super) fn parent(pid: u32) -> Option<u32> {
        let short = info::<libc::proc_bsdshortinfo>(pid, libc::PROC_PIDT_SHORTBSDINFO)?;
        (short.pbsi_ppid != 0).then_some(short.pbsi_ppid)
    }

    pub(super) fn group(group: u32) -> Option<Vec<u32>> {
        let group = libc::pid_t::try_from(group).ok()?;
        let mut pids: Vec<libc::pid_t> = vec![0; 64];
        loop {
            let size = libc::c_int::try_from(std::mem::size_of_val(pids.as_slice())).ok()?;
            // SAFETY: the buffer holds `size` writable bytes, and the call
            // returns how many pids it wrote, not how many bytes.
            let count = unsafe { libc::proc_listpgrppids(group, pids.as_mut_ptr().cast(), size) };
            let count = usize::try_from(count).ok()?;
            if count < pids.len() {
                pids.truncate(count);
                return Some(
                    pids.into_iter()
                        .filter_map(|pid| u32::try_from(pid).ok())
                        .filter(|pid| *pid != 0)
                        .collect(),
                );
            }
            pids.resize(pids.len() * 2, 0);
        }
    }

    pub(super) fn executable(pid: u32) -> Option<std::path::PathBuf> {
        let pid = libc::c_int::try_from(pid).ok()?;
        let mut buffer = vec![0_u8; usize::try_from(libc::PROC_PIDPATHINFO_MAXSIZE).ok()?];
        // SAFETY: proc_pidpath writes at most the given size into `buffer`
        // and returns the length of the path it wrote.
        let written = unsafe {
            libc::proc_pidpath(
                pid,
                buffer.as_mut_ptr().cast(),
                u32::try_from(buffer.len()).ok()?,
            )
        };
        let written = usize::try_from(written).ok().filter(|n| *n > 0)?;
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
        Some(OsStr::from_bytes(&buffer[..written]).into())
    }

    fn info<T>(pid: u32, flavor: libc::c_int) -> Option<T> {
        let pid = libc::pid_t::try_from(pid).ok()?;
        let size = libc::c_int::try_from(std::mem::size_of::<T>()).ok()?;
        let mut info = MaybeUninit::<T>::uninit();
        // SAFETY: proc_pidinfo writes at most `size` bytes into `info`, and
        // only a return value equal to `size` means the struct was filled.
        let written = unsafe { libc::proc_pidinfo(pid, flavor, 0, info.as_mut_ptr().cast(), size) };
        // SAFETY: the call above filled every byte of the struct.
        (written == size).then(|| unsafe { info.assume_init() })
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{LocalProc, LocalRead};
    use crate::mux::Incarnation;
    use std::io;

    struct Stat {
        state: char,
        parent: u32,
        group: u32,
        start_ticks: u64,
    }

    /// `/proc/<pid>/stat` is `pid (comm) state ppid pgrp ...`, and comm may
    /// hold spaces and parentheses, so the fields start after the last `)`.
    fn stat(pid: u32) -> io::Result<Stat> {
        let text = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
        let malformed = || io::Error::other(format!("/proc/{pid}/stat is malformed"));
        let (_, fields) = text.rsplit_once(')').ok_or_else(malformed)?;
        let fields: Vec<&str> = fields.split_whitespace().collect();
        let number = |index: usize| fields.get(index).and_then(|field| field.parse().ok());
        Ok(Stat {
            state: fields
                .first()
                .and_then(|state| state.chars().next())
                .ok_or_else(malformed)?,
            parent: number(1).ok_or_else(malformed)?,
            group: number(2).ok_or_else(malformed)?,
            // Field 22 of the line, counting pid as 1.
            start_ticks: fields
                .get(19)
                .and_then(|field| field.parse().ok())
                .ok_or_else(malformed)?,
        })
    }

    fn boot_id() -> Option<String> {
        let id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
        Some(id.trim().to_owned()).filter(|id| !id.is_empty())
    }

    pub(super) fn read(pid: u32) -> LocalRead {
        match stat(pid) {
            Ok(stat) if matches!(stat.state, 'Z' | 'X') => LocalRead::Gone,
            Ok(stat) => match boot_id() {
                Some(boot) => LocalRead::Alive(LocalProc {
                    incarnation: Incarnation(format!("{pid}@{boot}:{}", stat.start_ticks)),
                    group: stat.group,
                }),
                None => LocalRead::Unreadable,
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => LocalRead::Gone,
            Err(_) => LocalRead::Unreadable,
        }
    }

    pub(super) fn parent(pid: u32) -> Option<u32> {
        let parent = stat(pid).ok()?.parent;
        (parent != 0).then_some(parent)
    }

    pub(super) fn group(group: u32) -> Option<Vec<u32>> {
        let mut members = Vec::new();
        for entry in std::fs::read_dir("/proc").ok()?.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse().ok())
            else {
                continue;
            };
            if stat(pid).is_ok_and(|stat| stat.group == group) {
                members.push(pid);
            }
        }
        members.sort_unstable();
        Some(members)
    }

    pub(super) fn executable(pid: u32) -> Option<std::path::PathBuf> {
        std::fs::read_link(format!("/proc/{pid}/exe")).ok()
    }
}

/// Other systems are read by calls this code does not make, so no process
/// there reads as alive, and no Rex block there is typed into.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
mod platform {
    use super::LocalRead;

    pub(super) fn read(_pid: u32) -> LocalRead {
        LocalRead::Unreadable
    }

    pub(super) fn parent(_pid: u32) -> Option<u32> {
        None
    }

    pub(super) fn group(_group: u32) -> Option<Vec<u32>> {
        None
    }

    pub(super) fn executable(_pid: u32) -> Option<std::path::PathBuf> {
        None
    }
}

#[cfg(test)]
mod tests;
