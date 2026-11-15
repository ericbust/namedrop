use crate::mux::processes::{LocalProcesses, LocalRead, ThisMachine};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub(crate) fn stop(state: &Path) -> Result<(), String> {
    let paths = locks(state);
    if paths.is_empty() {
        return Ok(());
    }
    let local = ThisMachine;
    for pid in holders(&paths) {
        if pid <= 1 || pid == std::process::id() {
            continue;
        }
        let LocalRead::Alive(before) = local.read(pid) else {
            continue;
        };
        let Some(executable) = local.executable(pid) else {
            continue;
        };
        let releases = state.join("runtime/releases");
        if !executable.starts_with(fs::canonicalize(&releases).unwrap_or(releases))
            || executable.file_name().is_none_or(|name| name != "namedrop")
        {
            continue;
        }
        let Some((argv, environment)) = arguments(pid) else {
            continue;
        };
        if !helper(&argv, &environment, state) {
            continue;
        }
        if local.read(pid) != LocalRead::Alive(before.clone())
            || local.executable(pid).as_ref() != Some(&executable)
            || arguments(pid).as_ref() != Some(&(argv.clone(), environment.clone()))
        {
            continue;
        }
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
        let until = std::time::Instant::now() + std::time::Duration::from_secs(3);
        while local.read(pid) == LocalRead::Alive(before.clone())
            && std::time::Instant::now() < until
        {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if local.read(pid) == LocalRead::Alive(before)
            && local.executable(pid).as_ref() == Some(&executable)
            && arguments(pid).as_ref() == Some(&(argv, environment))
        {
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }
    Ok(())
}

fn locks(state: &Path) -> Vec<PathBuf> {
    fn walk(path: &Path, result: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                walk(&path, result);
            } else if kind.is_file() && path.extension().is_some_and(|e| e == "lock") {
                result.push(path);
            }
        }
    }
    let mut result = Vec::new();
    for name in [
        "rex-bridge",
        "relay",
        "deferred-commands",
        "agent-status-reconciliation",
        "dashboard",
    ] {
        walk(&state.join(name), &mut result);
    }
    let clock = state.join("reminder-clock.lock");
    if clock.is_file() {
        result.push(clock);
    }
    result
}

#[cfg(target_os = "macos")]
fn holders(paths: &[PathBuf]) -> Vec<u32> {
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if count <= 0 {
        return Vec::new();
    }
    let mut pids = vec![0_i32; count as usize + 256];
    let read = unsafe {
        libc::proc_listallpids(
            pids.as_mut_ptr().cast(),
            std::mem::size_of_val(pids.as_slice()) as i32,
        )
    };
    if read <= 0 {
        return Vec::new();
    }
    pids.truncate(read as usize);
    pids.into_iter()
        .filter_map(|pid| u32::try_from(pid).ok())
        .filter(|pid| {
            paths
                .iter()
                .any(|path| crate::mux::processes::opens(*pid, path))
        })
        .collect()
}

#[cfg(not(target_os = "macos"))]
fn holders(paths: &[PathBuf]) -> Vec<u32> {
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let pid = e.file_name().to_str()?.parse::<u32>().ok()?;
            let fds = fs::read_dir(e.path().join("fd")).ok()?;
            fds.flatten()
                .any(|fd| {
                    fs::read_link(fd.path())
                        .ok()
                        .is_some_and(|p| paths.contains(&p))
                })
                .then_some(pid)
        })
        .collect()
}

fn helper(argv: &[String], env: &BTreeMap<String, String>, state: &Path) -> bool {
    let rest: Vec<_> = argv.iter().skip(1).map(String::as_str).collect();
    let same = |p: &str| super::packages::same(Path::new(p), state);
    if matches!(rest.as_slice(), ["reminder-clock" | "--deferred-worker", s, _] if same(s)) {
        return true;
    }
    let mut words = Vec::new();
    let mut explicit = None;
    let mut i = 0;
    while i < rest.len() {
        if rest[i] == "--plugin-state" {
            explicit = rest.get(i + 1).copied();
            i += 2;
        } else if let Some(path) = rest[i].strip_prefix("--plugin-state=") {
            explicit = Some(path);
            i += 1;
        } else if matches!(rest[i], "--endpoint" | "--host" | "--ui") {
            i += 2;
        } else if rest[i] == "--inherit-claim" {
            i += 1;
        } else {
            words.push(rest[i]);
            i += 1;
        }
    }
    let state_matches = explicit
        .or_else(|| env.get("NAMEDROP_STATE_DIR").map(String::as_str))
        .or_else(|| env.get("HERDR_PLUGIN_STATE_DIR").map(String::as_str))
        .is_some_and(same);
    state_matches
        && matches!(
            words.as_slice(),
            ["rex", "bridge"]
                | ["relay", "worker"]
                | ["hook", "agent-status-worker"]
                | ["dashboard-server"]
        )
}

#[cfg(target_os = "macos")]
fn arguments(pid: u32) -> Option<(Vec<String>, BTreeMap<String, String>)> {
    let mut argmax: libc::c_int = 0;
    let mut size = std::mem::size_of_val(&argmax);
    let mut max_mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
    let status = unsafe {
        libc::sysctl(
            max_mib.as_mut_ptr(),
            2,
            (&mut argmax as *mut libc::c_int).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 || argmax <= 0 {
        return None;
    }
    let mut bytes = vec![0u8; usize::try_from(argmax).ok()?];
    size = bytes.len();
    let mut mib = [
        libc::CTL_KERN,
        libc::KERN_PROCARGS2,
        i32::try_from(pid).ok()?,
    ];
    if unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            bytes.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    } != 0
    {
        return None;
    }
    bytes.truncate(size);
    let argc = usize::try_from(i32::from_ne_bytes(bytes.get(..4)?.try_into().ok()?)).ok()?;
    let rest = bytes.get(4..)?;
    let rest = rest.get(rest.iter().position(|b| *b == 0)?..)?;
    let rest = rest
        .iter()
        .position(|b| *b != 0)
        .and_then(|i| rest.get(i..))?;
    let mut strings = rest.split(|b| *b == 0);
    let argv = strings
        .by_ref()
        .take(argc)
        .map(|b| String::from_utf8(b.to_vec()).ok())
        .collect::<Option<Vec<_>>>()?;
    let env = strings
        .take_while(|b| !b.is_empty())
        .filter_map(|b| {
            String::from_utf8(b.to_vec())
                .ok()?
                .split_once('=')
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
        })
        .collect();
    Some((argv, env))
}

#[cfg(not(target_os = "macos"))]
fn arguments(pid: u32) -> Option<(Vec<String>, BTreeMap<String, String>)> {
    let path = PathBuf::from(format!("/proc/{pid}"));
    let argv = fs::read(path.join("cmdline"))
        .ok()?
        .split(|b| *b == 0)
        .filter(|b| !b.is_empty())
        .map(|b| String::from_utf8(b.to_vec()).ok())
        .collect::<Option<Vec<_>>>()?;
    let env = fs::read(path.join("environ"))
        .ok()?
        .split(|b| *b == 0)
        .filter_map(|b| {
            String::from_utf8(b.to_vec())
                .ok()?
                .split_once('=')
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
        })
        .collect();
    Some((argv, env))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_helpers_of_the_exact_state_can_be_stopped() {
        let words = |s: &str| s.split_whitespace().map(str::to_owned).collect::<Vec<_>>();
        let env = BTreeMap::from([("NAMEDROP_STATE_DIR".into(), "/old".into())]);
        for args in [
            "namedrop rex bridge --endpoint rex:/tmp/r.sock",
            "namedrop relay worker --host b",
            "namedrop hook agent-status-worker",
            "namedrop dashboard-server",
            "namedrop --deferred-worker /old rex:/tmp/r.sock",
            "namedrop reminder-clock /old rex:/tmp/r.sock",
        ] {
            assert!(helper(&words(args), &env, Path::new("/old")), "{args}");
            assert!(!helper(&words(args), &env, Path::new("/other")), "{args}");
        }
        for args in [
            "namedrop ui run tour",
            "namedrop activate",
            "namedrop hook stop-worker --state-directory /old",
            "namedrop --plugin-state /new rex bridge",
        ] {
            assert!(!helper(&words(args), &env, Path::new("/old")), "{args}");
        }
    }
}
