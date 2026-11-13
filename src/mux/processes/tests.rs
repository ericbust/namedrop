use super::*;
use std::{os::unix::process::CommandExt, process::Command, thread, time::Duration};

fn alive(pid: u32) -> LocalProc {
    match ThisMachine.read(pid) {
        LocalRead::Alive(process) => process,
        other => panic!("process {pid} reads {other:?}"),
    }
}

#[test]
fn this_process_reads_its_parent_group_executable_and_a_stable_start() {
    let pid = std::process::id();
    // SAFETY: getppid and getpgrp only read this process's own ids.
    let (parent, group) = unsafe { (libc::getppid(), libc::getpgrp()) };
    assert_eq!(
        ThisMachine.parent(pid),
        Some(u32::try_from(parent).unwrap())
    );
    let first = alive(pid);
    assert_eq!(first.group, u32::try_from(group).unwrap());
    assert_eq!(alive(pid), first, "the start is read the same each time");
    assert!(
        first.incarnation.0.starts_with(&format!("{pid}@")),
        "{first:?}"
    );
    assert_eq!(
        ThisMachine
            .executable(pid)
            .map(std::fs::canonicalize)
            .transpose()
            .unwrap(),
        Some(std::env::current_exe().unwrap().canonicalize().unwrap())
    );
    assert!(
        ThisMachine
            .list_group(first.group)
            .is_some_and(|members| members.contains(&pid))
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_linux_incarnation_names_the_boot() {
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").unwrap();
    let incarnation = alive(std::process::id()).incarnation.0;
    assert!(
        incarnation.contains(&format!("@{}:", boot.trim())),
        "{incarnation}"
    );
}

/// Root's processes, such as the `login` under a Rex block's shell, may
/// exist and still be unreadable here.
#[cfg(target_os = "macos")]
#[test]
fn another_users_process_is_unreadable_not_gone() {
    assert_eq!(ThisMachine.read(1), LocalRead::Unreadable);
    assert_eq!(ThisMachine.parent(1), None, "launchd has no parent");
}

#[test]
fn a_pid_with_no_process_is_gone() {
    // Above every pid_max this machine allows.
    assert_eq!(ThisMachine.read(99_999_999), LocalRead::Gone);
}

/// A child that has exited and that this process never waits for stays a
/// zombie, which still has its pid and group.
#[test]
fn a_child_that_exited_and_was_not_reaped_is_gone_and_no_member_of_its_group() {
    let mut zombie = Command::new("/bin/sh")
        .args(["-c", "exit 0"])
        .process_group(0)
        .spawn()
        .unwrap();
    let pid = zombie.id();
    let mut state = ThisMachine.read(pid);
    for _ in 0..200 {
        if state == LocalRead::Gone {
            break;
        }
        thread::sleep(Duration::from_millis(10));
        state = ThisMachine.read(pid);
    }
    assert!(
        is_zombie(pid),
        "the child is still a zombie, so the read saw one"
    );
    assert_eq!(state, LocalRead::Gone);
    let members = members(&ThisMachine, pid).unwrap_or_default();
    assert!(!members.contains(&pid), "{members:?}");
    zombie.wait().unwrap();
}

/// A launcher that never reaps its agent keeps the dead agent as a zombie
/// in the launcher's live group, which the system still lists.
#[test]
fn a_zombie_in_a_live_group_is_listed_and_is_no_member() {
    let mut launcher = Command::new("/bin/sh")
        .args(["-c", "/bin/sh -c 'exit 0' & echo $!; exec /bin/sleep 30"])
        .process_group(0)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    std::io::BufRead::read_line(
        &mut std::io::BufReader::new(launcher.stdout.take().unwrap()),
        &mut line,
    )
    .unwrap();
    let agent: u32 = line.trim().parse().unwrap();
    let group = launcher.id();
    for _ in 0..200 {
        if is_zombie(agent) {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        is_zombie(agent),
        "the agent is a zombie of the live launcher"
    );
    assert_eq!(ThisMachine.read(agent), LocalRead::Gone);
    let listed = ThisMachine.list_group(group).expect("the group lists");
    assert!(
        listed.contains(&agent),
        "the system lists the zombie: {listed:?}"
    );
    assert_eq!(members(&ThisMachine, group), Some(vec![group]));
    launcher.kill().unwrap();
    launcher.wait().unwrap();
}

fn is_zombie(pid: u32) -> bool {
    let state = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    String::from_utf8_lossy(&state.stdout)
        .trim_start()
        .starts_with('Z')
}

#[test]
fn a_shell_is_named_by_its_executable() {
    for (path, shell) in [
        ("/bin/zsh", true),
        ("/bin/bash", true),
        ("/usr/bin/dash", true),
        ("/bin/busybox", true),
        ("/usr/local/bin/fish", true),
        ("/bin/sh", true),
        ("/usr/bin/python3", false),
        ("/usr/bin/login", false),
        ("/opt/claude/versions/2.1.323", false),
        ("/usr/bin/zshrc", false),
    ] {
        assert_eq!(is_shell(Path::new(path)), shell, "{path}");
    }
}
