#[path = "support/environment.rs"]
mod environment;
use environment::CommandHome;
use std::{fs, path::Path, process::Command};

#[test]
fn isolated_home_removes_ambient_socket_and_keeps_explicit_override() {
    let home = Path::new("/private/fixture-home");
    let mut command = Command::new("/bin/true");
    command
        .env("HC_SOCKET", "/ambient/core.sock")
        .isolated_home(home);
    assert_eq!(
        command.get_envs().find(|(key, _)| *key == "HC_SOCKET"),
        Some((std::ffi::OsStr::new("HC_SOCKET"), None))
    );
    assert!(
        command
            .get_envs()
            .any(|(key, value)| key == "XDG_RUNTIME_DIR"
                && value == Some(home.join(".runtime").as_os_str()))
    );
    command.env("HC_SOCKET", home.join("core.sock"));
    assert!(command.get_envs().any(
        |(key, value)| key == "HC_SOCKET" && value == Some(home.join("core.sock").as_os_str())
    ));
}

fn raw_environment_reset(source: &str) -> bool {
    source
        .split_whitespace()
        .collect::<String>()
        .contains(concat!(".env_", "clear("))
}

#[test]
fn reset_guard_detects_late_clears() {
    assert!(raw_environment_reset(concat!("command.env_", "clear()")));
    assert!(raw_environment_reset(concat!(
        "command.isolated_home(home) . env_",
        "clear ()"
    )));
    assert!(!raw_environment_reset(
        "command.isolated_env(home).env(\"HC_SOCKET\", socket)"
    ));
}

#[test]
fn test_commands_only_clear_environment_through_shared_helper() {
    fn inspect(root: &Path, violations: &mut Vec<String>) {
        for entry in fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                inspect(&path, violations);
            } else if path.extension().is_some_and(|extension| extension == "rs")
                && !path.ends_with("tests/support/environment.rs")
                && raw_environment_reset(&fs::read_to_string(&path).unwrap())
            {
                violations.push(path.display().to_string());
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut violations = Vec::new();
    inspect(&root.join("src"), &mut violations);
    inspect(&root.join("tests"), &mut violations);
    assert!(
        violations.is_empty(),
        "use CommandHome::isolated_env so a reset retains private HOME/XDG directories: {violations:#?}"
    );
}
