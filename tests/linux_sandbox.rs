#![cfg(target_os = "linux")]
//! Actual kernel enforcement, with no model/provider calls or recorder wrapper.
#[path = "support/cleanup.rs"]
mod cleanup;

use namedrop::linux_sandbox;
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};

const CANARY: &str = r#"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <errno.h>
#include <unistd.h>
#include <sys/socket.h>
#include <sys/prctl.h>
#include <sys/wait.h>
#include <sys/stat.h>
#include <netinet/in.h>
#include <arpa/inet.h>
int main(int argc, char **argv) {
    if (argc < 2) return 70;
    if (!strcmp(argv[1], "bootstrap")) {
        const char *dirs[] = {"sqlite", "thread-writer-locks", "cache", "log"};
        char path[4096];
        for (int i = 0; i < 4; i++) {
            snprintf(path, sizeof(path), "%s/%s", getenv("CODEX_HOME"), dirs[i]);
            if (mkdir(path, 0700) && errno != EEXIST) return 88;
            strcat(path, "/new-runtime-file");
            FILE *file = fopen(path, "w+"); if (!file) return 89;
            fputs("ok", file); rewind(file);
            if (fgetc(file) != 'o') return 90;
            fclose(file);
        }
        const char *files[] = {"auth.lock", "installation_id"};
        for (int i = 0; i < 2; i++) {
            snprintf(path, sizeof(path), "%s/%s", getenv("CODEX_HOME"), files[i]);
            FILE *file = fopen(path, "r+"); if (!file) return 91;
            fputs("ok", file); rewind(file);
            if (fgetc(file) != 'o') return 92;
            fclose(file);
        }
        return 0;
    }
    if (!strcmp(argv[1], "refresh")) {
        // Codex 0.156.1 refreshes auth.json by truncating the existing inode.
        FILE *file = fopen(argv[2], "w+"); if (!file) return 93;
        fputs("ok-refreshed", file); rewind(file);
        if (fgetc(file) != 'o') return 94;
        fclose(file); return 0;
    }
    if (!strcmp(argv[1], "npm")) {
        const char *root = getenv("CODEX_MANAGED_PACKAGE_ROOT");
        const char *manager = getenv("CODEX_MANAGED_BY_NPM");
        return root && !strcmp(root, argv[2]) && manager && !strcmp(manager, "1") ? 0 : 95;
    }
    if (!strcmp(argv[1], "system-file")) {
        FILE *file = fopen(argv[2], "r"); if (!file) return 86;
        fclose(file); return 0;
    }
    if (!strcmp(argv[1], "resolve")) {
        /* Resolve localhost the way the static Codex binary does: from /etc/hosts.
           Static glibc getaddrinfo would load the host's NSS modules and a second
           libc, which crashes on systems that list systemd resolvers. */
        FILE *hosts = fopen("/etc/hosts", "r"); if (!hosts) return 87;
        char line[512]; int found = 0;
        while (!found && fgets(line, sizeof(line), hosts)) {
            char *comment = strchr(line, '#'); if (comment) *comment = 0;
            char *save = NULL, *address = strtok_r(line, " \t\n", &save);
            struct in6_addr parsed;
            if (!address || (inet_pton(AF_INET, address, &parsed) != 1 && inet_pton(AF_INET6, address, &parsed) != 1)) continue;
            for (char *name; (name = strtok_r(NULL, " \t\n", &save));)
                if (!strcmp(name, "localhost")) { found = 1; break; }
        }
        fclose(hosts); return found ? 0 : 87;
    }
    if (!strcmp(argv[1], "nnp")) return prctl(PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) == 1 ? 0 : 81;
    if (!strcmp(argv[1], "fork-read")) {
        pid_t child = fork(); if (child < 0) return 82;
        if (child == 0) {
            FILE *file = fopen(argv[2], "r");
            _exit(!file && errno == EACCES ? 0 : 83);
        }
        int status; if (waitpid(child, &status, 0) < 0) return 84;
        return WIFEXITED(status) ? WEXITSTATUS(status) : 85;
    }
    if (!strcmp(argv[1], "read")) {
        FILE *file = fopen(argv[2], "r");
        if (!strcmp(argv[3], "allow")) {
            if (!file) return 71;
            int c = fgetc(file); fclose(file); return c == 'o' ? 0 : 72;
        }
        if (file) { fclose(file); return 73; }
        return errno == EACCES ? 0 : 74;
    }
    if (!strcmp(argv[1], "exec")) {
        execl(argv[2], argv[2], "-c", "exit 99", NULL);
        return errno == EACCES ? 0 : 75;
    }
    if (!strcmp(argv[1], "self")) { execl(argv[0], argv[0], "ok", NULL); return 76; }
    if (!strcmp(argv[1], "ok")) return 0;
    if (!strcmp(argv[1], "write")) {
        FILE *file = fopen(argv[2], "w"); if (!file) return 77;
        fputs("output", file); fclose(file); return 0;
    }
    if (!strcmp(argv[1], "network")) {
        int fd = socket(AF_INET, SOCK_STREAM, 0); if (fd < 0) return 78;
        struct sockaddr_in address = { .sin_family = AF_INET, .sin_port = htons(atoi(argv[2])), .sin_addr.s_addr = htonl(0x7f000001) };
        int result = connect(fd, (struct sockaddr *)&address, sizeof(address));
        close(fd); return result == 0 ? 0 : 79;
    }
    return 80;
}
"#;

#[test]
fn real_policy_allows_worker_auth_and_transport_but_denies_other_reads_and_exec() {
    let root = std::env::temp_dir().join(format!("namedrop-landlock-{}", std::process::id()));

    let _cleanup = cleanup::FixtureDirectory(root.clone());
    fs::create_dir_all(root.join("bin")).unwrap();
    if std::env::var_os("NAMEDROP_SANDBOX_FIXTURE_PANIC").is_some() {
        fs::write(
            root.join("pane-effects-retries.log"),
            "sandbox retry evidence\n",
        )
        .unwrap();
        panic!("intentional sandbox fixture panic");
    }
    fs::create_dir_all(root.join("worker")).unwrap();
    fs::create_dir_all(root.join("home/.codex")).unwrap();
    let binary = root.join("bin/codex");
    fs::write(root.join("probe.c"), CANARY).unwrap();
    let built = Command::new("cc")
        .args(["-static", "-O2", "-o"])
        .arg(&binary)
        .arg(root.join("probe.c"))
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "static canary build: {}",
        String::from_utf8_lossy(&built.stderr)
    );
    let worker = root.join("worker");
    let home = root.join("home/.codex");
    let run = |args: &[&str]| {
        let output = linux_sandbox::command(&binary, &worker, &home)
            .expect("Landlock must be enabled for Linux acceptance")
            .current_dir(&worker)
            .args(args)
            .output()
            .expect("contained canary spawn");
        assert!(
            output.status.success(),
            "canary {args:?}: {:?}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run(&["bootstrap"]);
    for name in ["sqlite", "thread-writer-locks", "cache", "log"] {
        assert_eq!(
            fs::metadata(home.join(name)).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    for name in ["auth.lock", "installation_id"] {
        assert_eq!(
            fs::metadata(home.join(name)).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::write(home.join(name), "existing-content").unwrap();
    }
    run(&["ok"]);
    for name in ["auth.lock", "installation_id"] {
        assert_eq!(
            fs::read_to_string(home.join(name)).unwrap(),
            "existing-content"
        );
    }
    for path in [
        worker.join("input"),
        home.join("auth.json"),
        home.join("cache/runtime"),
    ] {
        fs::write(&path, "ok").unwrap();
        run(&["read", path.to_str().unwrap(), "allow"]);
    }
    run(&["refresh", home.join("auth.json").to_str().unwrap()]);
    assert_eq!(
        fs::read_to_string(home.join("auth.json")).unwrap(),
        "ok-refreshed"
    );
    fs::write(
        root.join("codex-package.json"),
        r#"{"layoutVersion":1,"entrypoint":"bin/codex"}"#,
    )
    .unwrap();
    fs::create_dir(root.join("codex-resources")).unwrap();
    fs::write(root.join("codex-resources/control"), "ok").unwrap();
    run(&[
        "read",
        root.join("codex-resources/control").to_str().unwrap(),
        "allow",
    ]);
    fs::write(
        root.join("codex-package.json"),
        r#"{"layoutVersion":1,"entrypoint":"bin/different-codex"}"#,
    )
    .unwrap();
    run(&[
        "read",
        root.join("codex-resources/control").to_str().unwrap(),
        "deny",
    ]);
    for path in [
        root.join("unrelated-temp"),
        root.join("bin/private"),
        root.join("home/private"),
        home.join("history.jsonl"),
        home.join("AGENTS.md"),
        root.join("project"),
    ] {
        fs::write(&path, "ok").unwrap();
        run(&["read", path.to_str().unwrap(), "deny"]);
        // The parent's thread/process was not sandboxed along with its child.
        assert_eq!(fs::read_to_string(path).unwrap(), "ok");
    }
    let other = root.join("bin/other");
    fs::copy(&binary, &other).unwrap();
    for path in [
        Path::new("/bin/sh"),
        Path::new("/usr/bin/env"),
        other.as_path(),
    ] {
        run(&["exec", path.to_str().unwrap()]);
    }
    for path in ["/lib64/ld-linux-x86-64.so.2", "/lib/ld-linux-aarch64.so.1"] {
        if Path::new(path).exists() {
            run(&["exec", path]);
        }
    }
    run(&["self"]);
    run(&["nnp"]);
    run(&["fork-read", root.join("home/private").to_str().unwrap()]);
    let output = worker.join("result");
    run(&["write", output.to_str().unwrap()]);
    assert_eq!(fs::read_to_string(output).unwrap(), "output");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    run(&[
        "network",
        &listener.local_addr().unwrap().port().to_string(),
    ]);
    run(&["resolve"]);
    if Path::new("/etc/resolv.conf").exists() {
        run(&["system-file", "/etc/resolv.conf"]);
    }

    // npm resolution must grant the native binary, never its JS interpreter.
    let package = root.join("node_modules/@openai/codex");
    fs::create_dir_all(package.join("bin")).unwrap();
    let wrapper = package.join("bin/codex.js");
    fs::write(&wrapper, "#!/usr/bin/env node\n").unwrap();
    fs::write(package.join("package.json"), r#"{"name":"@openai/codex"}"#).unwrap();
    assert!(linux_sandbox::native_codex(&wrapper).is_err());
    let (platform, triple) = if cfg!(target_arch = "x86_64") {
        ("codex-linux-x64", "x86_64-unknown-linux-musl")
    } else {
        ("codex-linux-arm64", "aarch64-unknown-linux-musl")
    };
    let native = package
        .parent()
        .unwrap()
        .join(platform)
        .join("vendor")
        .join(triple)
        .join("bin/codex");
    fs::create_dir_all(native.parent().unwrap()).unwrap();
    fs::copy(&binary, &native).unwrap();
    assert_eq!(
        linux_sandbox::native_codex(&wrapper).unwrap(),
        native.canonicalize().unwrap()
    );
    let output = linux_sandbox::command(&wrapper, &worker, &home)
        .unwrap()
        .args(["npm", package.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(output.status.success(), "npm launch: {output:?}");
    let output = linux_sandbox::command(&wrapper, &worker, &home)
        .unwrap()
        .arg("system-file")
        .arg(package.join("package.json"))
        .output()
        .unwrap();
    assert!(output.status.success(), "npm manifest: {output:?}");
    assert!(linux_sandbox::native_codex(Path::new("/bin/sh")).is_err());
    assert!(linux_sandbox::command(Path::new("/bin/sh"), &worker, &home).is_err());
}

#[test]
fn sandbox_fixture_cleans_up_after_a_panic() {
    let child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "real_policy_allows_worker_auth_and_transport_but_denies_other_reads_and_exec",
            "--nocapture",
        ])
        .env("NAMEDROP_SANDBOX_FIXTURE_PANIC", "1")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let root = std::env::temp_dir().join(format!("namedrop-landlock-{}", child.id()));
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    let leaked = root.exists();
    if leaked {
        cleanup::remove_fixture(&root).unwrap();
    }
    assert!(
        !leaked,
        "panic left sandbox fixture behind: {}",
        root.display()
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("intentional sandbox fixture panic"),
        "{stderr}"
    );
    assert!(stderr.contains("sandbox retry evidence"), "{stderr}");
}
