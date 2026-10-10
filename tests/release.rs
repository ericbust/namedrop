use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "namedrop-release-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn script(&self, name: &str, body: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, body).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }
    fn command(&self, script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("scripts")
                .join(script),
        );
        command.env("PATH", format!("{}:/usr/bin:/bin", self.0.display()));
        command.env("PYTHONPATH", &self.0);
        command.current_dir(&self.0);
        command
    }
    fn host(&self) {
        self.script("socket.py", include_str!("setup_socket.py"));
        self.script("herdr", r#"#!/bin/sh
if [ -n "${NAMEDROP_TEST_LOG:-}" ]; then
  printf '%s\n' "$HERDR_SOCKET_PATH" "$HERDR_WORKSPACE_ID" "$HERDR_PANE_ID" "$@" >> "$NAMEDROP_TEST_LOG"
fi
if [ "${NAMEDROP_TEST_HERDR_STATUS:-0}" != 0 ]; then exit "$NAMEDROP_TEST_HERDR_STATUS"; fi
case "$1 $2 $3" in
  'pane list ')
    if [ "${NAMEDROP_TEST_PANE_GONE:-}" = 1 ]; then
      printf '%s\n' '{"result":{"panes":[]}}'
    else
      printf '%s\n' '{"result":{"panes":[{"pane_id":"pane-1","workspace_id":"space-1"}]}}'
    fi;;
  'plugin list --plugin') printf '%s\n' '{"result":{"plugins":[{"plugin_id":"namedrop","source":{"kind":"local"}}]}}';;
  'plugin link --enabled') printf '%s\n' 'unknown option: PATH' >&2; exit 2;;
  'plugin action invoke') printf '%s\n' '{"result":{"type":"plugin_action_invoked","log":{"log_id":"requested-log"}}}';;
  'plugin log list')
    if [ -n "${NAMEDROP_TEST_RUNNING_ONCE:-}" ] && [ ! -e "$NAMEDROP_TEST_RUNNING_ONCE" ]; then
      : > "$NAMEDROP_TEST_RUNNING_ONCE"
      printf '%s\n' '{"result":{"type":"plugin_log_list","logs":[{"log_id":"requested-log","status":"running"}]}}'
    else
      printf '%s\n' "$NAMEDROP_TEST_LOG_RESPONSE"
    fi;;
esac
"#);
    }
    /// Stands in for the release executable: it reports the package version and
    /// records how the packaged Herdr manifest invokes it.
    fn stand_in_binary(&self, name: &str, version: &str) -> PathBuf {
        self.script(
            name,
            &format!(
                r#"#!/bin/sh
if [ "${{1:-}}" = --version ]; then printf 'namedrop {version}\n'; exit 0; fi
printf '%s\n' "$*" >> "${{NAMEDROP_TEST_ENTRYPOINT_LOG:-/dev/null}}"
"#
            ),
        )
    }
    /// Makes the stubbed `file` describe the executable as `description`.
    fn reports(&self, description: &str) {
        self.script(
            "file",
            &format!("#!/bin/sh\nprintf '%s\\n' '{description}'\n"),
        );
    }
    fn package_release(&self, target: &str, binary: &Path, output: &Path) -> Output {
        self.command("package-release.sh")
            .args([
                "--target",
                target,
                "--binary",
                binary.to_str().unwrap(),
                "--output",
                output.to_str().unwrap(),
            ])
            .output()
            .unwrap()
    }
    /// Builds the Apple silicon release archive, verifies its external checksum
    /// and extracts it into `into`, which may contain spaces. Returns the
    /// archive it built and the extracted package directory.
    fn release(&self, into: &Path) -> (PathBuf, PathBuf) {
        self.release_for(TARGETS[0], into)
    }
    fn release_for(&self, (target, description): (&str, &str), into: &Path) -> (PathBuf, PathBuf) {
        let binary = self.stand_in_binary("fake namedrop", env!("CARGO_PKG_VERSION"));
        self.reports(description);
        let dist = self.0.join(format!("release output {target}"));
        success(self.package_release(target, &binary, &dist));
        assert!(sha256_check(
            &dist,
            &format!("{}.tar.gz.sha256", release_name(target))
        ));
        let archive = dist.join(format!("{}.tar.gz", release_name(target)));
        fs::create_dir_all(into).unwrap();
        success(
            Command::new("tar")
                .arg("-xzf")
                .arg(&archive)
                .arg("-C")
                .arg(into)
                .output()
                .unwrap(),
        );
        (archive, into.join(release_name(target)))
    }
    /// A copy of the source scripts, so a stand-in build stages its output in
    /// this fixture rather than in the repository under test.
    fn source_setup(&self) -> (PathBuf, Command) {
        let checkout = self.0.join("source checkout");
        fs::create_dir_all(checkout.join("scripts")).unwrap();
        for script in ["build.sh", "setup.sh", "run.sh"] {
            fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("scripts")
                    .join(script),
                checkout.join("scripts").join(script),
            )
            .unwrap();
        }
        let checkout = checkout.canonicalize().unwrap();
        self.script("cargo", CARGO);
        let mut command = Command::new("/bin/sh");
        command
            .arg(checkout.join("scripts/setup.sh"))
            .env("PATH", format!("{}:/usr/bin:/bin", self.0.display()))
            .env("PYTHONPATH", &self.0)
            .current_dir(&self.0);
        (checkout, command)
    }
    /// Runs the packaged setup helper against the fake host. A `cargo` that
    /// records its own invocation sits on PATH: a prebuilt package must never
    /// reach it.
    fn install(&self, package: &Path, log: &Path) -> Output {
        self.host();
        self.script(
            "cargo",
            "#!/bin/sh\nprintf 'cargo-build\\n' >> \"$NAMEDROP_TEST_LOG\"\nexit 99\n",
        );
        Command::new("sh")
            .arg(package.join("scripts/setup.sh"))
            .env("PATH", format!("{}:/usr/bin:/bin", self.0.display()))
            .env("PYTHONPATH", &self.0)
            .env("HERDR_SOCKET_PATH", "/isolated socket")
            .env("HERDR_WORKSPACE_ID", "space-1")
            .env("HERDR_PANE_ID", "pane-1")
            .env("NAMEDROP_TEST_LOG", log)
            .env(
                "NAMEDROP_TEST_LOG_RESPONSE",
                action_response("activate", 0, &activation_output(true)),
            )
            .output()
            .unwrap()
    }
}
/// Every release target with what `file` reports for a matching executable,
/// taken from real macOS and glibc builds.
const TARGETS: [(&str, &str); 3] = [
    ("aarch64-apple-darwin", "Mach-O 64-bit executable arm64"),
    (
        "x86_64-unknown-linux-gnu",
        "ELF 64-bit LSB pie executable, x86-64, version 1 (SYSV), dynamically linked, interpreter /lib64/ld-linux-x86-64.so.2, for GNU/Linux 3.2.0, stripped",
    ),
    (
        "aarch64-unknown-linux-gnu",
        "ELF 64-bit LSB pie executable, ARM aarch64, version 1 (SYSV), dynamically linked, interpreter /lib/ld-linux-aarch64.so.1, for GNU/Linux 3.7.0, stripped",
    ),
];
/// Stands in for a successful source build: records it, then writes Cargo's
/// release output where the build script stages it from.
const CARGO: &str = r#"#!/bin/sh
printf 'cargo-build\n' >> "${NAMEDROP_TEST_LOG:-/dev/null}"
while [ "$#" -gt 0 ]; do
  case "$1" in --target) target=$2;; --target-dir) directory=$2;; esac
  shift
done
mkdir -p "$directory/$target/release"
printf '#!/bin/sh\n' > "$directory/$target/release/namedrop"
chmod 755 "$directory/$target/release/namedrop"
"#;
/// Verifies a checksum list the way a user would on either platform: GNU/Linux
/// ships sha256sum, macOS ships shasum.
fn sha256_check(directory: &Path, list: &str) -> bool {
    let run = |program: &str, args: &[&str]| {
        Command::new(program)
            .args(args)
            .args(["-c", list])
            .current_dir(directory)
            .output()
    };
    match run("sha256sum", &[]) {
        Ok(output) => output.status.success(),
        Err(_) => run("shasum", &["-a", "256"]).unwrap().status.success(),
    }
}
fn action_response(action: &str, code: u8, stdout: &str) -> String {
    serde_json::json!({"result":{"type":"plugin_log_list","logs":[
        {"log_id":"unrelated-log","status":"succeeded","exit_code":0,"stdout":"UNRELATED"},
        {"log_id":"requested-log","plugin_id":"namedrop","action_id":action,
         "status":if code == 0 {"succeeded"} else {"failed"},"exit_code":code,
         "stdout":stdout,"stderr":""}
    ]}})
    .to_string()
}
fn activation_output(installed: bool) -> String {
    serde_json::json!({"data":{"space_id":"space-1","integration_packages":{"codex":{"installed":installed},"claude_code":{"installed":true}},"state_directory":"/fixture/state","setup_steps":["Trust the hooks"]}}).to_string()
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn setup_refuses_to_choose_a_workspace_or_change_state_outside_herdr() {
    let fixture = Fixture::new();
    let log = fixture.0.join("calls");
    fixture.script(
        "herdr",
        "#!/bin/sh\nprintf invoked >> \"$NAMEDROP_TEST_LOG\"\n",
    );
    let output = fixture
        .command("setup.sh")
        .env_remove("HERDR_WORKSPACE_ID")
        .env_remove("HERDR_PANE_ID")
        .env("NAMEDROP_TEST_LOG", &log)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("terminal in the desired Herdr workspace")
    );
    assert!(
        !log.exists(),
        "setup contacted Herdr before a target was supplied"
    );
}

#[test]
fn setup_requires_native_hook_python_before_mutating_herdr() {
    let fixture = Fixture::new();
    let log = fixture.0.join("calls");
    fixture.script(
        "herdr",
        "#!/bin/sh\nprintf invoked >> \"$NAMEDROP_TEST_LOG\"\n",
    );
    fixture.script(
        "uname",
        "#!/bin/sh\ncase \"$1\" in -s) printf Darwin;; -m) printf arm64;; esac\n",
    );
    let output = fixture
        .command("setup.sh")
        .env("PATH", &fixture.0)
        .env("HERDR_SOCKET_PATH", "/isolated socket")
        .env("HERDR_WORKSPACE_ID", "space-1")
        .env("HERDR_PANE_ID", "pane-1")
        .env("NAMEDROP_TEST_LOG", &log)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(69));
    assert!(String::from_utf8_lossy(&output.stderr).contains("require Python 3"));
    assert!(
        !log.exists(),
        "setup changed Herdr before prerequisites passed"
    );
}

#[test]
fn setup_uses_herdr_actions_preserves_context_and_never_relinks_on_check() {
    let fixture = Fixture::new();
    let log = fixture.0.join("calls");
    fixture.host();
    fixture.script(
        "uname",
        "#!/bin/sh\ncase \"$1\" in -s) printf Darwin;; -m) printf arm64;; esac\n",
    );
    let (checkout, mut command) = fixture.source_setup();
    command
        .env("HERDR_SOCKET_PATH", "/isolated socket")
        .env("HERDR_WORKSPACE_ID", "space-1")
        .env("HERDR_PANE_ID", "pane-1")
        .env("NAMEDROP_TEST_LOG", &log)
        .env(
            "NAMEDROP_TEST_LOG_RESPONSE",
            action_response("activate", 0, &activation_output(true)),
        );
    let output = success(command.output().unwrap());
    assert!(output.contains("Trust the hooks"));
    assert!(output.contains("Namedrop activated in space space-1."));
    assert!(!output.contains("\"data\""), "{output}");
    assert!(!output.contains("UNRELATED"));
    let calls = fs::read_to_string(&log).unwrap();
    // The registration is read before the build so a managed installation is
    // never rebuilt; the build still precedes linking a local checkout.
    assert!(calls.starts_with("/isolated socket\nspace-1\npane-1\nplugin\nlist\n"));
    let (before_link, _) = calls.split_once("plugin\nlink\n").unwrap();
    assert!(before_link.contains("cargo-build\n"));
    assert!(calls.contains(&format!(
        "plugin\nlink\n{}\n--enabled\n",
        checkout.display()
    )));
    assert!(checkout.join("bin/namedrop").is_file());
    assert!(calls.contains("plugin\naction\ninvoke\nactivate\n--plugin\nnamedrop\n"));
    assert!(calls.contains("integration\nstatus\n"));
    assert!(!calls.contains("integration\ninstall\n"));
    assert_eq!(
        calls.matches("/isolated socket\nspace-1\npane-1\n").count(),
        6
    );
    fs::write(&log, "").unwrap();
    let output = command
        .arg("check")
        .env("NAMEDROP_TEST_HERDR_STATUS", "7")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        fs::read_to_string(log).unwrap(),
        "/isolated socket\nspace-1\npane-1\npane\nlist\n"
    );
}

#[test]
fn setup_binds_action_to_origin_and_refuses_a_missing_pane() {
    let fixture = Fixture::new();
    fixture.host();
    let request = fixture.0.join("request.json");
    let calls = fixture.0.join("calls");
    let mut command = fixture.command("setup.sh");
    command
        .args(["activate", "--json"])
        .env("HERDR_SOCKET_PATH", "/isolated socket")
        .env("HERDR_WORKSPACE_ID", "space-1")
        .env("HERDR_PANE_ID", "pane-1")
        .env("NAMEDROP_TEST_SOCKET_REQUEST", &request)
        .env("NAMEDROP_TEST_LOG", &calls)
        .env(
            "NAMEDROP_TEST_LOG_RESPONSE",
            action_response("activate", 0, &activation_output(true)),
        );
    success(command.output().unwrap());
    let sent: serde_json::Value = serde_json::from_slice(&fs::read(&request).unwrap()).unwrap();
    assert_eq!(sent["method"], "plugin.action.invoke");
    assert_eq!(sent["params"]["context"]["workspace_id"], "space-1");
    assert_eq!(sent["params"]["context"]["focused_pane_id"], "pane-1");
    assert!(!fs::read_to_string(&calls).unwrap().contains("focus"));
    fs::remove_file(&request).unwrap();
    fs::write(&calls, "").unwrap();
    let output = command
        .env("NAMEDROP_TEST_PANE_GONE", "1")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("originating pane"));
    assert!(!request.exists());
    assert!(!fs::read_to_string(calls).unwrap().contains("invoke"));
}

#[test]
fn check_waits_for_the_exact_action_and_returns_the_health_exit_status() {
    let fixture = Fixture::new();
    fixture.host();
    for code in [0, 1, 2] {
        let status = ["healthy", "needs_attention", "unavailable"][usize::from(code)];
        let report = format!("{{\"data\":{{\"status\":\"{status}\"}}}}");
        let mut response: serde_json::Value =
            serde_json::from_str(&action_response("check", code, &report)).unwrap();
        response["result"]["logs"][1]["stderr"] = serde_json::json!("diagnostic detail\n");
        let output = fixture
            .command("setup.sh")
            .arg("check")
            .arg("--json")
            .env("HERDR_SOCKET_PATH", "/isolated socket")
            .env("HERDR_WORKSPACE_ID", "space-1")
            .env("HERDR_PANE_ID", "pane-1")
            .env(
                "NAMEDROP_TEST_RUNNING_ONCE",
                fixture.0.join(format!("once-{code}")),
            )
            .env("NAMEDROP_TEST_LOG_RESPONSE", response.to_string())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(i32::from(code)));
        assert!(String::from_utf8_lossy(&output.stdout).contains(&report));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("UNRELATED"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("diagnostic detail"));
    }
}

#[test]
fn check_never_calls_an_empty_or_malformed_report_healthy() {
    let fixture = Fixture::new();
    fixture.host();
    for report in [
        "",
        "not-json",
        "{}",
        r#"{"data":{"status":"needs_attention"}}"#,
    ] {
        let output = fixture
            .command("setup.sh")
            .arg("check")
            .env("HERDR_SOCKET_PATH", "/isolated socket")
            .env("HERDR_WORKSPACE_ID", "space-1")
            .env("HERDR_PANE_ID", "pane-1")
            .env(
                "NAMEDROP_TEST_LOG_RESPONSE",
                action_response("check", 0, report),
            )
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "report: {report}");
    }
}

#[test]
fn setup_json_is_parseable_and_human_checks_keep_issue_targets_and_repairs() {
    let fixture = Fixture::new();
    fixture.host();
    let mut command = fixture.command("setup.sh");
    command
        .arg("activate")
        .arg("--json")
        .env("HERDR_SOCKET_PATH", "/isolated socket")
        .env("HERDR_WORKSPACE_ID", "space-1")
        .env("HERDR_PANE_ID", "pane-1")
        .env(
            "NAMEDROP_TEST_LOG_RESPONSE",
            action_response("activate", 0, &activation_output(true)),
        );
    let output = command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["data"]["space_id"], "space-1");
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("Checking integrations")
    );

    let report = serde_json::json!({"data":{"status":"needs_attention", "available_agents":2, "waiting_messages":1,
        "issues":[{"code":"ND-DELIVERY-001", "summary":"A delivery needs attention", "explanation":"Later messages wait.",
          "diagnostic":{"delivery_id":"delivery-123"}, "repair":{"prompt":"Review before cancelling.",
            "command":["/a path/namedrop", "delivery", "cancel-held", "--input-json", "{\"delivery_id\":\"delivery-123\",\"confirmed\":true}"]}}]}});
    let output = fixture
        .command("setup.sh")
        .arg("check")
        .env("HERDR_SOCKET_PATH", "/isolated socket")
        .env("HERDR_WORKSPACE_ID", "space-1")
        .env("HERDR_PANE_ID", "pane-1")
        .env(
            "NAMEDROP_TEST_LOG_RESPONSE",
            action_response("check", 1, &report.to_string()),
        )
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(
        text.contains("Namedrop needs attention")
            && text.contains("delivery-123")
            && text.contains("Review before cancelling"),
        "{text}"
    );
    assert!(text.contains("'/a path/namedrop' delivery cancel-held"));
    assert!(!text.contains("\"data\""));
}

#[test]
fn setup_reports_partial_installation_and_malformed_action_results() {
    let fixture = Fixture::new();
    fixture.host();
    fixture.script(
        "uname",
        "#!/bin/sh\ncase \"$1\" in -s) printf Darwin;; -m) printf arm64;; esac\n",
    );
    let (_, mut command) = fixture.source_setup();
    command
        .env("HERDR_SOCKET_PATH", "/isolated socket")
        .env("HERDR_WORKSPACE_ID", "space-1")
        .env("HERDR_PANE_ID", "pane-1")
        .env(
            "NAMEDROP_TEST_LOG_RESPONSE",
            action_response("activate", 0, &activation_output(false)),
        );
    let output = command.output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stdout).contains("Trust the hooks"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("codex"));
    let output = command
        .env("NAMEDROP_TEST_LOG_RESPONSE", "not-json")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("could not verify"));
}

fn release_name(target: &str) -> String {
    format!("namedrop-{}-{target}", env!("CARGO_PKG_VERSION"))
}

/// Every Herdr entrypoint a manifest declares, as `kind:id` with its command.
fn manifest_entrypoints(manifest: &toml_edit::DocumentMut) -> Vec<(String, Vec<String>)> {
    let mut declared = Vec::new();
    for kind in ["startup", "actions", "events", "panes"] {
        let tables = manifest
            .get(kind)
            .unwrap_or_else(|| panic!("manifest declares no [[{kind}]] entrypoint"))
            .as_array_of_tables()
            .unwrap();
        for entry in tables {
            let id = entry
                .get("id")
                .or_else(|| entry.get("on"))
                .and_then(|value| value.as_str())
                .unwrap_or(kind);
            let command = entry["command"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap().to_owned())
                .collect();
            declared.push((format!("{kind}:{id}"), command));
        }
    }
    declared
}

/// Every regular file in the extracted package, relative to its root.
fn extracted_files(package: &Path) -> Vec<String> {
    fn walk(directory: &Path, package: &Path, found: &mut Vec<String>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, package, found);
            } else {
                found.push(path.strip_prefix(package).unwrap().display().to_string());
            }
        }
    }
    let mut found = Vec::new();
    walk(package, package, &mut found);
    found
}

#[test]
fn the_archive_bundles_every_declared_entrypoint_including_the_naming_popup() {
    let fixture = Fixture::new();
    let (_, package) = fixture.release(&fixture.0);

    let expected_files = [
        "bin/namedrop",
        "herdr-plugin.toml",
        "runtime.json",
        "scripts/setup.sh",
        "scripts/update.sh",
        "scripts/update.py",
        "scripts/run.sh",
        "scripts/runtime.py",
        "scripts/check-linux-prerequisites.sh",
        "SHA256SUMS",
        "README.md",
        "CONTRIBUTING.md",
        "LICENSE",
        "docs/operations.md",
        "docs/compatibility.md",
        "docs/releasing.md",
    ];
    for path in expected_files {
        assert!(package.join(path).is_file(), "missing archive file: {path}");
    }
    assert_eq!(
        extracted_files(&package).len(),
        expected_files.len(),
        "the archive contains unexpected files"
    );
    for path in [
        "Cargo.toml",
        "Cargo.lock",
        "src",
        "tests",
        "scripts/build.sh",
    ] {
        assert!(
            !package.join(path).exists(),
            "the archive ships a source-build input: {path}"
        );
    }
    for path in ["bin/namedrop", "scripts/setup.sh"] {
        assert_ne!(
            fs::metadata(package.join(path))
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0,
            "{path} is not executable after extraction"
        );
    }

    let distributed = fs::read_to_string(package.join("herdr-plugin.toml"))
        .unwrap()
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    assert!(
        distributed.get("build").is_none(),
        "the distributed manifest still requires a source build"
    );
    let source =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("herdr-plugin.toml"))
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
    let declared = manifest_entrypoints(&source);
    let bundled = manifest_entrypoints(&distributed);
    assert_eq!(
        declared.iter().map(|(id, _)| id).collect::<Vec<_>>(),
        bundled.iter().map(|(id, _)| id).collect::<Vec<_>>(),
        "the archive dropped or reordered a declared Herdr entrypoint"
    );
    for ((id, source_command), (_, bundled_command)) in declared.iter().zip(&bundled) {
        if id == "panes:setup" {
            assert_eq!(source_command, bundled_command);
            assert_eq!(
                bundled_command,
                &["sh", "./scripts/setup.sh", "activate", "--keep-open"]
            );
            continue;
        }
        if id == "actions:update" || id == "panes:update" {
            assert_eq!(source_command, bundled_command);
            assert_eq!(bundled_command[0..2], ["sh", "./scripts/update.sh"]);
            if id == "actions:update" {
                assert_eq!(bundled_command[2..], ["--open"]);
            } else {
                assert_eq!(bundled_command.len(), 2);
            }
            continue;
        }
        assert_eq!(&source_command[0..2], ["sh", "./scripts/run.sh"]);
        assert_eq!(&bundled_command[0..2], ["sh", "./scripts/run.sh"]);
        assert_eq!(
            source_command[1..],
            bundled_command[1..],
            "{id} arguments changed during packaging"
        );
    }
    let naming = distributed["panes"]
        .as_array_of_tables()
        .unwrap()
        .iter()
        .find(|pane| pane["id"].as_str() == Some("naming"))
        .expect("the archive omits the naming popup pane");
    assert_eq!(naming["placement"].as_str(), Some("popup"));
    let setup = distributed["panes"]
        .as_array_of_tables()
        .unwrap()
        .iter()
        .find(|pane| pane["id"].as_str() == Some("setup"))
        .expect("the archive omits the setup tab");
    assert_eq!(setup["placement"].as_str(), Some("tab"));

    // Every entrypoint's relative path must resolve to the bundled executable
    // from the extracted package, with Namedrop absent from PATH. `bin/namedrop`
    // is a stand-in that accepts any arguments, so this establishes resolution
    // and the argument vector each entrypoint receives -- not that the real
    // executable implements every subcommand.
    let invocations = fixture.0.join("entrypoint-calls");
    for (id, command) in &bundled {
        // Help resolves the setup script without activating a real Herdr workspace.
        let extra = if id == "panes:setup" || id == "panes:update" || id == "actions:update" {
            vec!["--help"]
        } else {
            vec![]
        };
        // Activation publication is covered with real processes in the runtime
        // lifecycle suite. This packaging check exercises its shipped parser.
        if id == "actions:activate" {
            assert!(package.join("scripts/runtime.py").is_file());
            continue;
        }
        assert!(
            Command::new("/bin/sh")
                .args(["-c", r#"exec "$@""#, "sh"])
                .args(command)
                .args(extra)
                .current_dir(&package)
                .env("PATH", "/usr/bin:/bin")
                .env_remove("HERDR_PLUGIN_STATE_DIR")
                // Herdr sets the pane for the invocation it starts; run.sh
                // passes it on explicitly so the runtime never treats it as
                // an inherited, possibly stale, pane.
                .env("HERDR_PANE_ID", "pane-1")
                .env("NAMEDROP_TEST_ENTRYPOINT_LOG", &invocations)
                .output()
                .unwrap()
                .status
                .success(),
            "{id} did not resolve to the bundled executable"
        );
    }
    let invoked = fs::read_to_string(&invocations).unwrap();
    assert_eq!(invoked.lines().count(), bundled.len() - 4);
    for expected in [
        "reconcile",
        "ui launch name",
        "ui launch manage",
        "ui launch doctor",
        "ui launch status",
        "ui run",
        "ui run manage",
        "ui run doctor",
        "ui run status",
        "doctor",
        "deactivate",
        "space pause",
        "space resume",
        "event pane-lifecycle",
        "hook agent-status-changed",
    ] {
        assert!(
            invoked
                .lines()
                .any(|line| line == format!("--herdr-pane pane-1 {expected}")),
            "no bundled entrypoint invoked `{expected}` with its explicit pane: {invoked}"
        );
    }

    // A released file with no internal checksum cannot be verified after download.
    let sums = fs::read_to_string(package.join("SHA256SUMS")).unwrap();
    let listed: Vec<&str> = sums
        .lines()
        .filter_map(|line| line.split_once("  "))
        .map(|(_, path)| path)
        .collect();
    let extracted = extracted_files(&package);
    for path in &extracted {
        assert!(
            path.as_str() == "SHA256SUMS" || listed.contains(&path.as_str()),
            "{path} ships without an internal checksum"
        );
    }
    assert_eq!(listed.len(), extracted.len() - 1);
}

#[test]
fn the_archive_installs_without_cargo_detects_tampering_and_is_never_overwritten() {
    let fixture = Fixture::new();
    let (archive, package) = fixture.release(&fixture.0);
    let log = fixture.0.join("calls");
    // The packaged copy of the helper must report the activation result too.
    let installed = success(fixture.install(&package, &log));
    assert!(installed.contains("Trust the hooks"));
    let calls = fs::read_to_string(&log).unwrap();
    assert!(
        !calls.contains("cargo-build"),
        "the prebuilt package invoked Cargo: {calls}"
    );
    // Herdr links the physical installation directory the helper resolved.
    let linked = package.canonicalize().unwrap();
    assert!(calls.contains(&format!("plugin\nlink\n{}\n--enabled\n", linked.display())));

    assert!(sha256_check(&package, "SHA256SUMS"));
    fs::write(package.join("README.md"), "tampered").unwrap();
    assert!(!sha256_check(&package, "SHA256SUMS"));

    let released = fs::read(&archive).unwrap();
    let binary = fixture.stand_in_binary("fake namedrop", env!("CARGO_PKG_VERSION"));
    let refused = fixture.package_release(TARGETS[0].0, &binary, archive.parent().unwrap());
    assert_eq!(refused.status.code(), Some(73));
    assert_eq!(fs::read(&archive).unwrap(), released);
}

#[test]
fn the_archive_installs_from_a_directory_containing_spaces() {
    let fixture = Fixture::new();
    let (_, package) = fixture.release(&fixture.0.join("Application Support/namedrop releases"));
    assert!(package.to_str().unwrap().contains(' '));
    assert!(sha256_check(&package, "SHA256SUMS"));

    let log = fixture.0.join("calls");
    success(fixture.install(&package, &log));
    let calls = fs::read_to_string(&log).unwrap();
    assert!(
        !calls.contains("cargo-build"),
        "the prebuilt package invoked Cargo: {calls}"
    );
    let linked = package.canonicalize().unwrap();
    assert!(
        linked.to_str().unwrap().contains(' '),
        "the resolved installation path lost its spaces"
    );
    assert!(
        calls.contains(&format!("plugin\nlink\n{}\n--enabled\n", linked.display())),
        "linking lost the spaced installation directory: {calls}"
    );

    let invocations = fixture.0.join("spaced-entrypoint-call");
    success(
        Command::new("/bin/sh")
            .args(["-c", r#"exec "$@""#, "sh", "./bin/namedrop", "reconcile"])
            .current_dir(&package)
            .env("PATH", "/usr/bin:/bin")
            .env("NAMEDROP_TEST_ENTRYPOINT_LOG", &invocations)
            .output()
            .unwrap(),
    );
    assert_eq!(fs::read_to_string(invocations).unwrap(), "reconcile\n");
}

#[test]
fn linux_archives_name_their_target_declare_linux_and_install_without_cargo() {
    let source =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("herdr-plugin.toml"))
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
    for release in &TARGETS[1..] {
        let fixture = Fixture::new();
        let (_, package) = fixture.release_for(*release, &fixture.0.join("linux releases"));
        assert!(package.ends_with(release_name(release.0)));
        assert!(sha256_check(&package, "SHA256SUMS"));
        let distributed = fs::read_to_string(package.join("herdr-plugin.toml"))
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert!(distributed.get("build").is_none());
        assert_eq!(
            distributed["platforms"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect::<Vec<_>>(),
            ["linux"],
            "{}: the archive must declare only its own platform",
            release.0
        );
        assert_eq!(
            manifest_entrypoints(&distributed),
            manifest_entrypoints(&source)
        );
        let log = fixture.0.join("calls");
        success(fixture.install(&package, &log));
        let calls = fs::read_to_string(&log).unwrap();
        assert!(!calls.contains("cargo-build"), "{calls}");
        assert!(calls.contains(&format!(
            "plugin\nlink\n{}\n--enabled\n",
            package.canonicalize().unwrap().display()
        )));
    }
    let fixture = Fixture::new();
    let (_, package) = fixture.release(&fixture.0);
    let distributed = fs::read_to_string(package.join("herdr-plugin.toml"))
        .unwrap()
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    assert_eq!(
        distributed["platforms"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<Vec<_>>(),
        ["macos"]
    );
}

#[test]
fn an_archive_for_another_platform_is_refused_before_it_is_linked() {
    let fixture = Fixture::new();
    let (_, package) = fixture.release(&fixture.0);
    // An executable format this machine cannot load, as with a Linux archive on
    // a Mac or an aarch64 archive on x86_64.
    let mut foreign = b"\x7fELF\x02\x01\x01".to_vec();
    foreign.resize(64, 0);
    fs::write(package.join("bin/namedrop"), foreign).unwrap();
    let log = fixture.0.join("calls");
    let refused = fixture.install(&package, &log);
    assert_eq!(refused.status.code(), Some(69));
    let message = String::from_utf8_lossy(&refused.stderr);
    assert!(
        message.contains("does not run on this machine"),
        "{message}"
    );
    assert!(message.contains("nothing was linked"), "{message}");
    let calls = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        !calls.contains("plugin\nlink\n") && !calls.contains("plugin\naction\n"),
        "a foreign archive was linked or activated: {calls}"
    );
}

#[test]
fn packaging_rejects_foreign_architecture_unexecutable_input_and_a_version_mismatch() {
    let fixture = Fixture::new();
    let dist = fixture.0.join("refused output");
    let matching = fixture.stand_in_binary("matching namedrop", env!("CARGO_PKG_VERSION"));

    // Each target accepts only its own format. A static musl build and a
    // Linux executable for another architecture are foreign too.
    let musl = "ELF 64-bit LSB executable, x86-64, version 1 (SYSV), statically linked, stripped";
    let foreign = [
        (TARGETS[0].0, "Mach-O 64-bit executable x86_64"),
        (TARGETS[0].0, TARGETS[1].1),
        (TARGETS[1].0, TARGETS[2].1),
        (TARGETS[1].0, musl),
        (TARGETS[1].0, TARGETS[0].1),
        (TARGETS[2].0, TARGETS[1].1),
    ];
    for (target, description) in foreign {
        fixture.reports(description);
        let refused = fixture.package_release(target, &matching, &dist);
        assert_eq!(refused.status.code(), Some(65), "{target}: {description}");
        assert!(
            String::from_utf8_lossy(&refused.stderr)
                .contains(&format!("not an executable for {target}")),
            "{target}: {}",
            String::from_utf8_lossy(&refused.stderr)
        );
    }
    let unsupported = fixture.package_release("x86_64-apple-darwin", &matching, &dist);
    assert_eq!(unsupported.status.code(), Some(64));
    assert!(String::from_utf8_lossy(&unsupported.stderr).contains("Unsupported release target"));

    fixture.reports(TARGETS[0].1);
    let unusable = fixture.0.join("not executable");
    fs::write(&unusable, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&unusable, fs::Permissions::from_mode(0o644)).unwrap();
    for candidate in [unusable, fixture.0.join("absent namedrop")] {
        let refused = fixture.package_release(TARGETS[0].0, &candidate, &dist);
        assert_eq!(refused.status.code(), Some(65));
        assert!(String::from_utf8_lossy(&refused.stderr).contains("must exist and be executable"));
    }

    let mismatched = fixture.stand_in_binary("mismatched namedrop", "9.9.9");
    let refused = fixture.package_release(TARGETS[0].0, &mismatched, &dist);
    assert_eq!(refused.status.code(), Some(65));
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("does not match Cargo.toml"),
        "unexpected rejection: {}",
        String::from_utf8_lossy(&refused.stderr)
    );

    assert_eq!(
        fs::read_dir(&dist).unwrap().count(),
        0,
        "a refused build left release output behind"
    );
}
