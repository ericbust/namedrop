use crate::cleanup;

use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

const ROOT: &str = "NAMEDROP_UPDATE_TEST_ROOT";

pub fn root(default: PathBuf) -> PathBuf {
    env::var_os(ROOT).map(PathBuf::from).unwrap_or(default)
}

pub fn isolate(root: &Path, home: &Path) -> bool {
    if env::var_os(ROOT).is_some() {
        return false;
    }
    let thread = std::thread::current();
    let output = Command::new(env::current_exe().unwrap())
        .args(["--exact", thread.name().unwrap(), "--nocapture"])
        .env(ROOT, root)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("CODEX_HOME", home.join(".codex"))
        .env("CLAUDE_CONFIG_DIR", home.join(".claude"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

pub fn cleanup(root: &Path) {
    if env::var_os(ROOT).is_none() {
        cleanup::remove_fixture(root).unwrap();
    }
}
