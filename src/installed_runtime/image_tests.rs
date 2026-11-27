use super::*;
use crate::mux::processes::{LocalProcesses, ThisMachine};
use std::{
    path::Path,
    process::{Child, Command},
    thread,
    time::{Duration, Instant},
};

const STATE: &str = "NAMEDROP_IMAGE_TEST_STATE";
const MODULE: &str = "installed_runtime::image_tests";

fn release(state: &Path, digit: &str) -> PathBuf {
    state
        .join("runtime/releases")
        .join(digit.repeat(64))
        .join("namedrop")
}

fn select(state: &Path, digit: &str) {
    let runtime = state.join("runtime");
    let next = runtime.join("next");
    std::os::unix::fs::symlink(release(state, digit).parent().unwrap(), &next).unwrap();
    fs::rename(next, runtime.join("current")).unwrap();
}

fn until(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !ready() {
        assert!(Instant::now() < deadline, "runtime image fixture timed out");
        thread::sleep(Duration::from_millis(10));
    }
}

struct Fixture {
    state: PathBuf,
    child: Option<Child>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.state);
    }
}

fn through_launcher(test: &str, check: impl FnOnce(&Path)) {
    if let Some(state) = std::env::var_os(STATE).map(PathBuf::from) {
        if !state.join("switched").is_file() {
            fs::write(state.join("ready"), []).unwrap();
            until(|| state.join("switched").is_file());
        }
        check(&state);
        return;
    }

    let mut fixture = Fixture {
        state: std::env::temp_dir().join(format!("namedrop-image-{}-{test}", std::process::id())),
        child: None,
    };
    let state = &fixture.state;
    let runtime = state.join("runtime");
    let old = release(state, "a");
    let new = release(state, "b");
    for path in [&old, &new] {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        crate::executable::copy_executable(std::env::current_exe().unwrap(), path);
    }
    let launcher = runtime.join("namedrop");
    let script = include_str!("../../scripts/runtime.py")
        .split_once("LAUNCHER = '''")
        .unwrap()
        .1
        .split_once("'''")
        .unwrap()
        .0;
    crate::executable::write_executable(&launcher, script);
    std::os::unix::fs::symlink(old.parent().unwrap(), runtime.join("current")).unwrap();
    fixture.child = Some(
        Command::new(&launcher)
            .args(["--exact", &format!("{MODULE}::{test}")])
            .env_clear()
            .env(STATE, state)
            .spawn()
            .unwrap(),
    );
    until(|| state.join("ready").is_file());
    select(state, "b");
    fs::write(state.join("switched"), []).unwrap();
    let mut status = None;
    until(|| {
        status = fixture.child.as_mut().unwrap().try_wait().unwrap();
        status.is_some()
    });
    fixture.child.take();
    assert!(status.unwrap().success(), "{test} child failed");
}

#[test]
fn executable_image_stays_with_the_running_release() {
    through_launcher("executable_image_stays_with_the_running_release", |state| {
        let old = release(state, "a");
        assert_eq!(executable_image().unwrap(), fs::canonicalize(&old).unwrap());
        let identity = ExecutableIdentity::read(&old).unwrap();
        assert_eq!(ExecutableIdentity::running().unwrap(), identity);
        assert!(!identity.matches(ExecutableIdentity::read(&release(state, "b")).ok()));
        #[cfg(target_os = "linux")]
        {
            fs::remove_file(old).unwrap();
            assert_eq!(ExecutableIdentity::running().unwrap(), identity);
        }
    });
}

#[test]
fn replaced_helper_runs_the_selected_image() {
    through_launcher("replaced_helper_runs_the_selected_image", |state| {
        reexec_if_replaced(state).unwrap();
        assert_eq!(
            ThisMachine.executable(std::process::id()),
            Some(fs::canonicalize(release(state, "b")).unwrap())
        );
    });
}

#[test]
fn a_startup_match_consumes_the_previous_exec_attempt() {
    through_launcher(
        "a_startup_match_consumes_the_previous_exec_attempt",
        |state| {
            let attempted = ExecutableIdentity::read(&release(state, "b")).unwrap();
            remember_reexec_attempt(Some(reexec_attempt_marker(attempted)));
            select(state, "a");
            assert!(replacement(state).unwrap().is_none());
            select(state, "b");
            assert!(replacement(state).unwrap().is_some());
        },
    );
}

#[test]
fn retrying_a_refused_exec_waits_for_a_different_selection() {
    through_launcher(
        "retrying_a_refused_exec_waits_for_a_different_selection",
        |state| {
            let attempted = ExecutableIdentity::read(&release(state, "b")).unwrap();
            remember_reexec_attempt(Some(reexec_attempt_marker(attempted)));
            assert!(replacement(state).unwrap_err().contains("still running"));
            assert!(replacement(state).unwrap_err().contains("still running"));
            select(state, "a");
            assert!(replacement(state).unwrap().is_none());
            select(state, "b");
            assert!(replacement(state).unwrap().is_some());
        },
    );
}
