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
        Some((
            std::ffi::OsStr::new("HC_SOCKET"),
            Some(std::ffi::OsStr::new(""))
        ))
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
        .contains(concat!("env_", "clear("))
}

#[test]
fn reset_guard_detects_late_clears() {
    assert!(raw_environment_reset(concat!("command.env_", "clear()")));
    assert!(raw_environment_reset(concat!(
        "command.isolated_home(home) . env_",
        "clear ()"
    )));
    assert!(raw_environment_reset(concat!(
        "Command::env_",
        "clear(&mut command)"
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

#[test]
fn isolated_env_clears_old_values_before_installing_private_directories() {
    let home = Path::new("/private/fixture-home");
    let mut command = Command::new("/usr/bin/env");
    command
        .env("HC_SOCKET", "/ambient/core.sock")
        .env("XDG_RUNTIME_DIR", "/ambient/runtime")
        .env("UNRELATED_SENTINEL", "must disappear")
        .isolated_env(home);
    let output = command.output().unwrap();
    assert!(output.status.success());
    let actual = String::from_utf8(output.stdout).unwrap();
    let values: std::collections::BTreeMap<_, _> = actual
        .lines()
        .map(|line| line.split_once('=').unwrap())
        .collect();
    for (key, suffix) in [
        ("HOME", ""),
        ("XDG_STATE_HOME", ".local/state"),
        ("XDG_CONFIG_HOME", ".config"),
        ("XDG_DATA_HOME", ".local/share"),
        ("XDG_CACHE_HOME", ".cache"),
        ("XDG_RUNTIME_DIR", ".runtime"),
    ] {
        assert_eq!(Path::new(values[key]), home.join(suffix));
    }
    assert!(!values.contains_key("UNRELATED_SENTINEL"));
    assert_eq!(values["HC_SOCKET"], "");
}

#[test]
fn python_home_overrides_ambient_socket_when_merged() {
    let output = Command::new("/usr/bin/python3")
        .isolated_env("/private/fixture-home")
        .env("HC_SOCKET", "/ambient/core.sock")
        .arg("-c")
        .arg("import os, sys; sys.path.insert(0, sys.argv[1]); from environment import isolated_home; env = dict(os.environ, **isolated_home('/private/fixture-home')); assert env['HC_SOCKET'] == ''; assert env['XDG_RUNTIME_DIR'] == '/private/fixture-home/.runtime'; assert env['XDG_CACHE_HOME'] == '/private/fixture-home/.cache'")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support"))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[path = "support/socket_fixture.rs"]
mod socket_fixture;

#[test]
fn socket_roots_budget_nested_paths_for_macos_and_long_tmpdirs() {
    use std::os::unix::ffi::OsStrExt;
    let macos = "/var/folders/zz/zyxvpxvq6csfxvn_n0000000000000/T";
    let private_macos = format!("/private{macos}");
    assert_eq!(macos.len(), 48);
    assert_eq!(private_macos.len(), 56);
    for base in [macos, &private_macos, &format!("/{}", "long".repeat(80))] {
        for sequence in [0, u64::MAX] {
            let root = socket_fixture::socket_root(Path::new(base), u32::MAX, sequence);
            let endpoint = root.join(".runtime/herdr-core/default.sock");
            assert!(
                endpoint.as_os_str().as_bytes().len() <= 103,
                "{}",
                endpoint.display()
            );
            if base.len() > 103 || sequence == u64::MAX {
                assert_eq!(root.parent(), Some(Path::new("/tmp")));
            }
        }
    }
}

#[test]
fn socket_budget_counts_utf8_bytes_and_accepts_exact_limit() {
    let exact = format!("/{}", "é".repeat(51));
    assert!(socket_fixture::socket_path_fits(Path::new(&exact), 103));
    assert!(!socket_fixture::socket_path_fits(
        Path::new(&(exact + "x")),
        103
    ));
}

#[test]
fn socket_roots_are_unique_private_directories() {
    use std::os::unix::fs::PermissionsExt;
    let first = socket_fixture::short_socket_root();
    let second = socket_fixture::short_socket_root();
    assert_ne!(first, second);
    for root in [first, second] {
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::remove_dir(root).unwrap();
    }
}

#[test]
#[should_panic(expected = "fixture command must explicitly clear HC_SOCKET")]
fn routing_guard_rejects_retained_ambient_socket_without_connecting() {
    let home = Path::new("/private/fixture-home");
    let mut command = Command::new("/bin/true");
    command
        .isolated_home(home)
        .env("HC_SOCKET", "/ambient/core.sock");
    environment::assert_private_hc_routing(&command, home);
}

#[test]
#[should_panic(expected = "fixture command must assign private XDG_RUNTIME_DIR")]
fn routing_guard_rejects_missing_runtime_without_connecting() {
    let home = Path::new("/private/fixture-home");
    let mut command = Command::new("/bin/true");
    command.isolated_home(home).env_remove("XDG_RUNTIME_DIR");
    environment::assert_private_hc_routing(&command, home);
}

#[test]
#[should_panic(expected = "fixture command must explicitly clear HC_SOCKET")]
fn routing_guard_rejects_missing_socket_override_without_connecting() {
    let home = Path::new("/private/fixture-home");
    let mut command = Command::new("/bin/true");
    command
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_RUNTIME_DIR", home.join(".runtime"));
    environment::assert_private_hc_routing(&command, home);
}
