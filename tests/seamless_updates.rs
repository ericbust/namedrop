#[path = "support/environment.rs"]
mod environment;
use environment::CommandHome;

#[test]
fn codex_activation_installs_and_verifies_the_local_plugin() {
    let output = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/codex_activation.py"
        ))
        .output()
        .expect("Python 3 is required for Codex activation verification");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn runtime_replacement_preserves_cached_tools_and_running_commands() {
    let output = std::process::Command::new("python3")
        .env("NAMEDROP_TEST_BINARY", env!("CARGO_BIN_EXE_namedrop"))
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/runtime_lifecycle.py"
        ))
        .output()
        .expect("Python 3 is required for runtime lifecycle verification");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn live_schema_migration_preserves_commands_and_recovers_interruption() {
    let output = std::process::Command::new("python3")
        .env("NAMEDROP_TEST_BINARY", env!("CARGO_BIN_EXE_namedrop"))
        .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/live_schema.py"))
        .output()
        .expect("Python 3 is required for live schema verification");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Gated migration runs everywhere. Historical ungated releases exist only on
    // macOS; elsewhere one must be refused rather than silently skipped.
    let history = if cfg!(target_os = "macos") {
        "LIVE_SCHEMA_LEGACY_MACOS_OK"
    } else {
        "LIVE_SCHEMA_UNGATED_REFUSED_OK"
    };
    for marker in [
        "LIVE_SCHEMA_GATED_OK",
        history,
        "LIVE_SCHEMA_PROCESS_AND_RECOVERY_OK",
    ] {
        assert!(stdout.contains(marker), "missing {marker}:\n{stdout}");
    }
}

/// The Linux preflight's verdict on stubbed host commands: a usable Landlock
/// ABI passes, an unusable one or a missing required tool fails, and an
/// unsupported architecture never issues the x86_64/aarch64-only syscall.
#[test]
fn linux_preflight_requires_landlock_and_named_prerequisites() {
    use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};
    let root = std::env::temp_dir().join(format!("namedrop-preflight-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let stub = |name: &str, body: &str| {
        let path = bin.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    };
    // Only the utilities the script itself needs, so no real host tool leaks in.
    for tool in ["awk", "cat", "date", "head", "sed", "tr"] {
        let real = ["/usr/bin", "/bin"]
            .iter()
            .map(|directory| Path::new(directory).join(tool))
            .find(|path| path.exists())
            .unwrap();
        std::os::unix::fs::symlink(real, bin.join(tool)).unwrap();
    }
    stub("getconf", "echo glibc 2.42");
    stub(
        "python3",
        r#"case "$1" in --version) echo 'Python 3.12.1';; -c) : > "$0.syscall"; printf '%s\n' "$FAKE_LANDLOCK";; esac"#,
    );
    for (name, version) in [
        ("herdr", "herdr 0.9.1"),
        ("git", "git version 2.51.0"),
        ("codex", "codex-cli 0.156.1"),
        ("claude", "2.1.282 (Claude Code)"),
    ] {
        stub(name, &format!("echo '{version}'"));
    }
    let run = |machine: &str, landlock: &str| {
        stub(
            "uname",
            &format!(
                r#"case "$1" in -s) echo Linux;; -m) echo {machine};; *) echo 7.2.1-1-omarchy;; esac"#
            ),
        );
        let _ = fs::remove_file(bin.join("python3.syscall"));
        let output = Command::new("/bin/sh")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/scripts/check-linux-prerequisites.sh"
            ))
            .isolated_env(&root)
            .env("PATH", &bin)
            .env("CLAUDE_CONFIG_DIR", root.join(".claude"))
            .env("CODEX_HOME", root.join(".codex"))
            .env("FAKE_LANDLOCK", landlock)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        (
            output.status.code(),
            stdout,
            bin.join("python3.syscall").exists(),
        )
    };

    let (code, out, probed) = run("x86_64", "3");
    assert!(code == Some(0) && probed, "{out}");
    assert!(out.contains("required prerequisites present"), "{out}");
    // An arbitrary wrapper (or /usr/bin ELF) must never trigger a broad find
    // from its parent. Only the known npm layout can be inspected that way.
    stub("readlink", "echo /usr/bin/codex");
    stub("find", ": > \"$0.called\"");
    for kind in ["POSIX shell script", "ELF dynamically linked"] {
        stub("file", &format!("echo '{kind}'"));
        let (code, out, _) = run("x86_64", "3");
        assert_eq!(code, Some(0), "{out}");
        assert!(
            !bin.join("find.called").exists(),
            "unexpected broad search: {out}"
        );
    }
    let (code, out, _) = run("aarch64", "unavailable (Operation not supported)");
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("missing required: landlock-abi>=1\n"), "{out}");
    let (code, out, probed) = run("riscv64", "3");
    assert!(
        code == Some(1) && !probed,
        "syscall issued on riscv64:\n{out}"
    );
    assert!(
        out.contains("missing required: architecture landlock-abi>=1\n"),
        "{out}"
    );
    fs::remove_file(bin.join("codex")).unwrap();
    let (code, out, _) = run("x86_64", "3");
    assert_eq!(code, Some(1), "{out}");
    assert!(out.contains("missing required: codex\n"), "{out}");
    fs::remove_dir_all(&root).unwrap();
}
