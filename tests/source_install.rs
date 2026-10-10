//! Command-level checks for the source-installation route: the manifest build
//! command Herdr runs before it registers the plugin, and the setup helper that
//! links a local checkout or activates an already registered installation.
//! Every fixture path contains a space so quoting is exercised throughout.
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// Where the source build stages its result; run.sh, activation and the
/// updater look here first.
const STAGED_EXECUTABLE: &str = "bin/namedrop";

/// Every supported build host, as `uname -s`, `uname -m` and the Rust target.
const PLATFORMS: [(&str, &str, &str); 3] = [
    ("Darwin", "arm64", "aarch64-apple-darwin"),
    ("Linux", "x86_64", "x86_64-unknown-linux-gnu"),
    ("Linux", "aarch64", "aarch64-unknown-linux-gnu"),
];

/// Base system tools live in /bin on macOS and /usr/bin on merged-/usr Linux.
fn system_tool(name: &str) -> PathBuf {
    ["/usr/bin", "/bin"]
        .iter()
        .map(|directory| Path::new(directory).join(name))
        .find(|path| path.exists())
        .unwrap_or_else(|| panic!("no system {name}"))
}

struct Fixture {
    root: PathBuf,
    tools: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "namedrop source install {}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let tools = root.join("stub tools");
        fs::create_dir_all(&tools).unwrap();
        fs::create_dir(root.join("home")).unwrap();
        let fixture = Self { root, tools };
        // The build script and stand-in cargo need only these base tools, so a
        // stub-only PATH can prove cargo and the C toolchain are absent.
        for tool in ["dirname", "mkdir", "cp", "mv", "rm", "chmod"] {
            std::os::unix::fs::symlink(system_tool(tool), fixture.tools.join(tool)).unwrap();
        }
        fixture.platform("Darwin", "arm64");
        fixture.stub("xcode-select", "#!/bin/sh\nexit 0\n");
        fixture
    }

    fn platform(&self, system: &str, machine: &str) {
        self.stub(
            "uname",
            &format!(
                "#!/bin/sh\ncase \"$1\" in -s) printf {system};; -m) printf {machine};; esac\n"
            ),
        );
    }

    /// The GNU/Linux build prerequisites: glibc and a C compiler and linker.
    fn linux_toolchain(&self) {
        self.stub("getconf", "#!/bin/sh\nprintf 'glibc 2.39\\n'\n");
        self.stub("cc", "#!/bin/sh\nexit 0\n");
    }

    fn stub(&self, name: &str, body: &str) -> PathBuf {
        let path = self.tools.join(name);
        fs::write(&path, body).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }

    /// A copy of the shipped manifest and scripts under a path with a space,
    /// standing in for an extracted source download or managed checkout.
    fn checkout(&self) -> PathBuf {
        let checkout = self.root.join("source checkout");
        fs::create_dir_all(checkout.join("scripts")).unwrap();
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
        fs::copy(
            repository.join("herdr-plugin.toml"),
            checkout.join("herdr-plugin.toml"),
        )
        .unwrap();
        for script in [
            "build.sh",
            "setup.sh",
            "update.sh",
            "update.py",
            "run.sh",
            "runtime.py",
        ] {
            fs::copy(
                repository.join("scripts").join(script),
                checkout.join("scripts").join(script),
            )
            .unwrap();
        }
        // The scripts resolve their own root with `pwd -P`, so compare against
        // the same fully resolved path rather than the temporary-directory link.
        fs::canonicalize(checkout).unwrap()
    }

    /// A second copy under a different path, standing in for the replacement
    /// installation a user extracts beside the one already registered.
    fn replacement_checkout(&self) -> PathBuf {
        let checkout = self.root.join("replacement checkout");
        fs::create_dir_all(checkout.join("scripts")).unwrap();
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
        fs::copy(
            repository.join("herdr-plugin.toml"),
            checkout.join("herdr-plugin.toml"),
        )
        .unwrap();
        for script in ["build.sh", "setup.sh"] {
            fs::copy(
                repository.join("scripts").join(script),
                checkout.join("scripts").join(script),
            )
            .unwrap();
        }
        fs::canonicalize(checkout).unwrap()
    }

    fn log(&self) -> PathBuf {
        self.root.join("host calls")
    }

    fn cargo_log(&self) -> PathBuf {
        self.root.join("cargo calls")
    }

    fn calls(&self) -> String {
        fs::read_to_string(self.log()).unwrap_or_default()
    }

    /// Records every argument on its own line, so a path containing a space
    /// stays distinguishable from two separate arguments.
    fn host(&self) {
        self.stub("socket.py", include_str!("setup_socket.py"));
        self.stub(
            "herdr",
            r#"#!/bin/sh
printf '%s\n' '--call--' "$HERDR_SOCKET_PATH" "$HERDR_WORKSPACE_ID" "$HERDR_PANE_ID" "$@" >> "$NAMEDROP_TEST_LOG"
case "$1 $2 $3" in
  'pane list ') printf '%s\n' '{"result":{"panes":[{"pane_id":"pane-1","workspace_id":"space with spaces"}]}}';;
  'plugin list --plugin')
    if [ "${NAMEDROP_TEST_LIST_STATUS:-0}" != 0 ]; then
      printf '%s\n' '{"error":{"code":"server_not_running"}}' >&2
      exit "$NAMEDROP_TEST_LIST_STATUS"
    fi
    printf '%s\n' "$NAMEDROP_TEST_LIST_RESPONSE";;
  'integration status') printf '%s\n' 'codex: installed' 'claude: installed';;
  'plugin link '*) printf '%s\n' 'linked';;
  'plugin action invoke') printf '%s\n' '{"result":{"type":"plugin_action_invoked","log":{"log_id":"requested-log"}}}';;
  'plugin log list') printf '%s\n' "$NAMEDROP_TEST_LOG_RESPONSE";;
  *) printf '%s\n' '{}';;
esac
"#,
        );
    }

    /// Records its arguments; a successful build writes a distinct stand-in
    /// executable where Cargo would put the release output.
    fn cargo(&self, status: u8) {
        self.stub(
            "cargo",
            &format!(
                r#"#!/bin/sh
printf '%s\n' "$@" >> "$NAMEDROP_TEST_CARGO_LOG"
[ {status} = 0 ] || exit {status}
while [ "$#" -gt 0 ]; do
  case "$1" in --target) target=$2;; --target-dir) directory=$2;; esac
  shift
done
mkdir -p "$directory/$target/release"
printf '#!/bin/sh\nprintf "built %s %s\\n"\n' "$target" "$$" > "$directory/$target/release/namedrop"
chmod 755 "$directory/$target/release/namedrop"
"#
            ),
        );
    }

    fn build(&self, checkout: &Path) -> Command {
        let mut command = Command::new("/bin/sh");
        command
            .arg(checkout.join("scripts/build.sh"))
            .env("PATH", &self.tools)
            .env("HOME", self.root.join("home"))
            .env("CLAUDE_CONFIG_DIR", self.root.join("home").join(".claude"))
            .env("CODEX_HOME", self.root.join("home").join(".codex"))
            .env_remove("XDG_CONFIG_HOME")
            .env("NAMEDROP_TEST_LOG", self.log())
            .env("NAMEDROP_TEST_CARGO_LOG", self.cargo_log())
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_WORKSPACE_ID")
            .env_remove("HERDR_PANE_ID")
            .env_remove("HERDR_PLUGIN_STATE_DIR")
            .current_dir(&self.root);
        command
    }

    fn setup(&self, checkout: &Path, operation: Option<&str>) -> Command {
        let mut command = Command::new("/bin/sh");
        command.arg(checkout.join("scripts/setup.sh"));
        if let Some(operation) = operation {
            command.arg(operation);
        }
        command
            .env("PATH", format!("{}:/usr/bin:/bin", self.tools.display()))
            .env("PYTHONPATH", &self.tools)
            .env("HOME", self.root.join("home"))
            .env("CLAUDE_CONFIG_DIR", self.root.join("home").join(".claude"))
            .env("CODEX_HOME", self.root.join("home").join(".codex"))
            .env_remove("XDG_CONFIG_HOME")
            .env("NAMEDROP_TEST_LOG", self.log())
            .env("NAMEDROP_TEST_CARGO_LOG", self.cargo_log())
            .env("NAMEDROP_TEST_LIST_RESPONSE", registration("local"))
            .env(
                "NAMEDROP_TEST_LOG_RESPONSE",
                action_log("activate", 0, &activation("space with spaces", true)),
            )
            .env("HERDR_SOCKET_PATH", "/isolated socket")
            .env("HERDR_WORKSPACE_ID", "space with spaces")
            .env("HERDR_PANE_ID", "pane-1")
            .current_dir(&self.root);
        command
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn registration(kind: &str) -> String {
    let source = match kind {
        "none" => return serde_json::json!({"result":{"plugins":[]}}).to_string(),
        "github" => serde_json::json!({
            "kind": "github",
            "repo": "unresolved-owner/unresolved-repo",
            "requested_ref": "unresolved-tag",
            "resolved_commit": "0000000000000000000000000000000000000000",
        }),
        other => serde_json::json!({"kind": other}),
    };
    serde_json::json!({"result":{"plugins":[
        {"plugin_id":"namedrop","enabled":true,"source":source}
    ]}})
    .to_string()
}

fn activation(space: &str, installed: bool) -> String {
    serde_json::json!({"data":{
        "space_id": space,
        "state_directory": "/isolated state/namedrop",
        "integration_packages": {"codex":{"installed":installed},"claude_code":{"installed":true}},
        "agent_integration_hook_activation": {"codex":{"status":"human_action_required","steps":["Trust the hooks"]}},
    }})
    .to_string()
}

fn action_log(action: &str, code: u8, stdout: &str) -> String {
    serde_json::json!({"result":{"type":"plugin_log_list","logs":[
        {"log_id":"requested-log","plugin_id":"namedrop","action_id":action,
         "status": if code == 0 {"succeeded"} else {"failed"},
         "exit_code": code, "stdout": stdout, "stderr": ""}
    ]}})
    .to_string()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn succeeded(output: &Output) -> String {
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        stdout(output),
        stderr(output)
    );
    stdout(output)
}

#[test]
fn source_build_needs_no_host_context_and_changes_nothing_it_does_not_own() {
    let fixture = Fixture::new();
    let checkout = fixture.checkout();
    fixture.host();
    fixture.stub(
        "rustup",
        "#!/bin/sh\nprintf 'rustup\\n' >> \"$NAMEDROP_TEST_LOG\"\n",
    );
    fixture.cargo(0);
    let manifest = checkout.join("herdr-plugin.toml");
    let previewed = fs::read(&manifest).unwrap();
    succeeded(&fixture.build(&checkout).output().unwrap());
    assert_eq!(
        fs::read(&manifest).unwrap(),
        previewed,
        "the build changed the manifest Herdr previewed"
    );
    assert_eq!(
        fixture.calls(),
        "",
        "the build contacted Herdr or installed a toolchain"
    );
    for shared in [".codex", ".claude", ".agents", ".config"] {
        assert!(
            !fixture.root.join("home").join(shared).exists(),
            "the build changed the shared profile at {shared}"
        );
    }
}

#[test]
fn source_build_stages_each_supported_target_where_every_entrypoint_looks() {
    for (system, machine, target) in PLATFORMS {
        let fixture = Fixture::new();
        let checkout = fixture.checkout();
        fixture.platform(system, machine);
        fixture.linux_toolchain();
        fixture.cargo(0);
        succeeded(
            &fixture
                .build(&checkout)
                .env("CARGO_TARGET_DIR", fixture.root.join("caller target"))
                .output()
                .unwrap(),
        );
        let invocation = fs::read_to_string(fixture.cargo_log()).unwrap();
        let argument = |flag: &str| {
            invocation
                .lines()
                .skip_while(|line| *line != flag)
                .nth(1)
                .unwrap_or_default()
                .to_owned()
        };
        assert!(invocation.contains("--locked\n"), "{invocation}");
        assert!(invocation.contains("--release\n"), "{invocation}");
        assert!(!invocation.contains("caller target"), "{invocation}");
        assert_eq!(argument("--target"), target, "{system} {machine}");
        assert_eq!(
            Path::new(&argument("--target-dir")),
            checkout.join("target")
        );
        let built = checkout
            .join("target")
            .join(target)
            .join("release/namedrop");
        let staged = checkout.join(STAGED_EXECUTABLE);
        assert_eq!(fs::read(&staged).unwrap(), fs::read(&built).unwrap());
        assert_ne!(
            fs::metadata(&staged).unwrap().permissions().mode() & 0o111,
            0,
            "{target}: the staged executable is not executable"
        );
        // Before activation, host entrypoints resolve through run.sh to it.
        let resolved = Command::new("/bin/sh")
            .args(["./scripts/run.sh", "reconcile"])
            .current_dir(&checkout)
            .env("PATH", "/usr/bin:/bin")
            .env_remove("HERDR_PLUGIN_STATE_DIR")
            .output()
            .unwrap();
        assert!(succeeded(&resolved).starts_with(&format!("built {target} ")));

        // A rebuild replaces the staged file by renaming a complete copy, so a
        // command still running the previous executable keeps its own file.
        let previous = fs::metadata(&staged).unwrap().ino();
        succeeded(&fixture.build(&checkout).output().unwrap());
        assert_ne!(fs::metadata(&staged).unwrap().ino(), previous);
        assert_eq!(fs::read(&staged).unwrap(), fs::read(&built).unwrap());
        assert_eq!(
            fs::read_dir(checkout.join("bin")).unwrap().count(),
            1,
            "{target}: staging left a temporary file behind"
        );
    }
}

#[test]
fn source_manifest_routes_every_entrypoint_through_the_launcher_scripts() {
    let fixture = Fixture::new();
    let checkout = fixture.checkout();
    let manifest = fs::read_to_string(checkout.join("herdr-plugin.toml"))
        .unwrap()
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
    let platforms = manifest["platforms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| value.as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(platforms, ["macos", "linux"]);
    for section in ["startup", "actions", "panes", "events"] {
        for entry in manifest[section].as_array_of_tables().unwrap() {
            if ["actions", "panes"].contains(&section) && entry["id"].as_str() == Some("update") {
                let command = entry["command"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|value| value.as_str().unwrap())
                    .collect::<Vec<_>>();
                let expected = if section == "actions" {
                    vec!["sh", "./scripts/update.sh", "--open"]
                } else {
                    assert_eq!(entry["placement"].as_str(), Some("tab"));
                    vec!["sh", "./scripts/update.sh"]
                };
                assert_eq!(command, expected);
                assert!(
                    Command::new("sh")
                        .args([command[1], "--help"])
                        .current_dir(&checkout)
                        .output()
                        .unwrap()
                        .status
                        .success()
                );
                assert!(checkout.join("scripts/update.py").is_file());
                continue;
            }
            if section == "panes" && entry["id"].as_str() == Some("setup") {
                assert_eq!(entry["placement"].as_str(), Some("tab"));
                assert_eq!(
                    entry["command"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|value| value.as_str().unwrap())
                        .collect::<Vec<_>>(),
                    ["sh", "./scripts/setup.sh", "activate", "--keep-open"]
                );
                continue;
            }
            let command = entry["command"].as_array().unwrap();
            assert_eq!(command.get(0).unwrap().as_str(), Some("sh"));
            assert_eq!(
                command.get(1).unwrap().as_str(),
                Some("./scripts/run.sh"),
                "{section} bypasses the runtime launcher"
            );
        }
    }
}

#[test]
fn source_build_reports_missing_tools_and_unsupported_architecture_without_installing_them() {
    let fixture = Fixture::new();
    let checkout = fixture.checkout();
    fixture.host();

    let missing_rust = fixture.build(&checkout).output().unwrap();
    assert_eq!(missing_rust.status.code(), Some(69));
    let message = stderr(&missing_rust);
    assert!(message.contains("Rust 1.97 or newer"), "{message}");
    assert!(message.contains("never installs a toolchain"), "{message}");

    fixture.cargo(0);
    for (system, machine) in [
        ("Darwin", "x86_64"),
        ("Linux", "riscv64"),
        ("FreeBSD", "amd64"),
    ] {
        fixture.platform(system, machine);
        let unsupported = fixture.build(&checkout).output().unwrap();
        assert_eq!(unsupported.status.code(), Some(69));
        let message = stderr(&unsupported);
        assert!(message.contains("macOS on Apple silicon"), "{message}");
        assert!(
            message.contains("GNU/Linux on x86_64 or aarch64"),
            "{message}"
        );
        assert!(
            message.contains(&format!("{system} {machine}")),
            "{message}"
        );
    }

    // GNU/Linux needs glibc and a C compiler and linker, checked in that order.
    fixture.platform("Linux", "x86_64");
    let musl = fixture.build(&checkout).output().unwrap();
    assert_eq!(musl.status.code(), Some(69));
    let message = stderr(&musl);
    assert!(message.contains("GNU C library"), "{message}");
    assert!(message.contains("Alpine are not supported"), "{message}");
    fixture.stub("getconf", "#!/bin/sh\nprintf 'glibc 2.39\\n'\n");
    let no_compiler = fixture.build(&checkout).output().unwrap();
    assert_eq!(no_compiler.status.code(), Some(69));
    let message = stderr(&no_compiler);
    assert!(message.contains("C compiler and linker"), "{message}");
    assert!(message.contains("base-devel"), "{message}");
    assert!(message.contains("build-essential"), "{message}");

    fixture.platform("Darwin", "arm64");
    fixture.stub("xcode-select", "#!/bin/sh\nexit 2\n");
    let missing_tools = fixture.build(&checkout).output().unwrap();
    assert_eq!(missing_tools.status.code(), Some(69));
    let message = stderr(&missing_tools);
    assert!(
        message.contains("command-line developer tools"),
        "{message}"
    );
    assert!(message.contains("xcode-select --install"), "{message}");

    assert!(
        !fixture.cargo_log().exists(),
        "an unsupported build still invoked cargo"
    );
    assert_eq!(
        fixture.calls(),
        "",
        "a failed prerequisite check contacted Herdr"
    );
}

#[test]
fn a_failed_source_build_is_never_reported_as_a_completed_installation() {
    let fixture = Fixture::new();
    let checkout = fixture.checkout();
    fixture.host();
    fixture.cargo(101);

    let build = fixture.build(&checkout).output().unwrap();
    assert_eq!(build.status.code(), Some(101));

    let install = fixture.setup(&checkout, None).output().unwrap();
    assert_eq!(install.status.code(), Some(101));
    let calls = fixture.calls();
    assert!(calls.contains("plugin\nlist\n"), "{calls}");
    assert!(
        !calls.contains("plugin\nlink\n") && !calls.contains("plugin\naction\n"),
        "a failed build still registered or activated Namedrop: {calls}"
    );
    assert!(
        !stdout(&install).contains("Next steps"),
        "a failed build reported completed setup"
    );
}

#[test]
fn installing_a_local_source_checkout_builds_links_and_activates_the_supplied_workspace() {
    // A first install has no registration at all; a later one already has a
    // local link. Both build this checkout and relink it to itself.
    for kind in ["none", "local"] {
        let fixture = Fixture::new();
        let checkout = fixture.checkout();
        fixture.host();
        fixture.cargo(0);
        let output = succeeded(
            &fixture
                .setup(&checkout, None)
                .env("NAMEDROP_TEST_LIST_RESPONSE", registration(kind))
                .output()
                .unwrap(),
        );
        assert!(output.contains("Next steps"), "{kind}: {output}");
        assert!(output.contains("Trust the hooks"), "{kind}: {output}");
        assert!(
            output.contains("/isolated state/namedrop"),
            "{kind}: {output}"
        );
        assert!(
            fs::read_to_string(fixture.cargo_log())
                .unwrap()
                .contains("--locked\n"),
            "{kind}: installation skipped the locked source build"
        );
        let calls = fixture.calls();
        assert!(
            calls.contains(&format!(
                "plugin\nlink\n{}\n--enabled\n",
                checkout.display()
            )),
            "{kind}: the checkout with a space was not linked verbatim: {calls}"
        );
        assert!(
            calls.contains("plugin\naction\ninvoke\nactivate\n--plugin\nnamedrop\n"),
            "{kind}: {calls}"
        );
        assert_eq!(
            calls
                .matches("--call--\n/isolated socket\nspace with spaces\npane-1\n")
                .count(),
            6,
            "{kind}: every host call must keep this terminal's Herdr context: {calls}"
        );
    }
}

#[test]
fn installing_over_a_managed_installation_retains_it_without_rebuilding_or_relinking() {
    let fixture = Fixture::new();
    let checkout = fixture.checkout();
    fixture.host();
    fixture.cargo(0);
    let output = succeeded(
        &fixture
            .setup(&checkout, None)
            .env("NAMEDROP_TEST_LIST_RESPONSE", registration("github"))
            .output()
            .unwrap(),
    );
    assert!(
        output.contains("Using the installed GitHub revision"),
        "{output}"
    );
    assert!(output.contains("activating this workspace"), "{output}");
    assert!(
        !fixture.cargo_log().exists(),
        "a managed installation was rebuilt"
    );
    let calls = fixture.calls();
    assert!(
        !calls.contains("plugin\nlink\n"),
        "a managed installation was relinked into a local link: {calls}"
    );
    assert!(
        calls.contains("plugin\naction\ninvoke\nactivate\n--plugin\nnamedrop\n"),
        "{calls}"
    );
}

#[test]
fn setup_tab_preserves_action_status_and_json_when_dismissed() {
    use std::io::Write;
    use std::process::Stdio;

    let fixture = Fixture::new();
    let checkout = fixture.checkout();
    fixture.host();
    for code in [0, 7] {
        let payload = activation("space with spaces", true);
        let mut child = fixture
            .setup(&checkout, Some("activate"))
            .args(["--keep-open", "--json"])
            .env(
                "NAMEDROP_TEST_LOG_RESPONSE",
                action_log("activate", code, &payload),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"\n").unwrap();
        let output = child.wait_with_output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(i32::from(code)),
            "{}",
            stderr(&output)
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
            serde_json::from_str::<serde_json::Value>(&payload).unwrap()
        );
        assert!(stderr(&output).contains("Press Enter when you are done reading."));
    }
    let missing_context = fixture
        .setup(&checkout, Some("activate"))
        .arg("--keep-open")
        .env_remove("HERDR_WORKSPACE_ID")
        .output()
        .unwrap();
    assert_eq!(missing_context.status.code(), Some(64));
    assert!(stderr(&missing_context).contains("Open a terminal in the desired Herdr workspace"));
    assert!(stderr(&missing_context).contains("Press Enter when you are done reading."));
}

#[test]
fn activate_only_never_builds_or_links_and_must_match_the_supplied_workspace() {
    let fixture = Fixture::new();
    let checkout = fixture.checkout();
    fixture.host();
    fixture.cargo(0);
    let output = succeeded(&fixture.setup(&checkout, Some("activate")).output().unwrap());
    assert!(output.contains("Next steps"), "{output}");
    assert!(
        !fixture.cargo_log().exists(),
        "explicit activation rebuilt the plugin"
    );
    let calls = fixture.calls();
    assert!(
        !calls.contains("plugin\nlink\n") && !calls.contains("plugin\nlist\n"),
        "explicit activation relinked or reinspected the registration: {calls}"
    );

    let other_space = fixture
        .setup(&checkout, Some("activate"))
        .env(
            "NAMEDROP_TEST_LOG_RESPONSE",
            action_log("activate", 0, &activation("another-space", true)),
        )
        .output()
        .unwrap();
    assert_eq!(other_space.status.code(), Some(2));
    let message = stderr(&other_space);
    assert!(message.contains("'another-space'"), "{message}");
    assert!(message.contains("'space with spaces'"), "{message}");
}

#[test]
fn activation_without_a_state_directory_or_readable_registration_is_not_reported_as_setup() {
    let fixture = Fixture::new();
    let checkout = fixture.checkout();
    fixture.host();
    fixture.cargo(0);

    let mut malformed: serde_json::Value =
        serde_json::from_str(&activation("space with spaces", true)).unwrap();
    malformed["data"]["state_directory"] = serde_json::json!("");
    let no_state = fixture
        .setup(&checkout, Some("activate"))
        .env(
            "NAMEDROP_TEST_LOG_RESPONSE",
            action_log("activate", 0, &malformed.to_string()),
        )
        .output()
        .unwrap();
    assert_eq!(no_state.status.code(), Some(2));
    assert!(
        stderr(&no_state).contains("no state directory"),
        "{}",
        stderr(&no_state)
    );

    let unreadable = fixture
        .setup(&checkout, None)
        .env("NAMEDROP_TEST_LIST_STATUS", "1")
        .output()
        .unwrap();
    assert_eq!(unreadable.status.code(), Some(2));
    let message = stderr(&unreadable);
    assert!(message.contains("how Namedrop is registered"), "{message}");
    assert!(message.contains("setup.sh activate"), "{message}");
    assert!(
        !fixture.cargo_log().exists() && !fixture.calls().contains("plugin\nlink\n"),
        "an unreadable registration was relinked anyway"
    );
}

#[test]
fn no_registration_answer_is_ever_mistaken_for_permission_to_relink() {
    // Every one of these once produced either a crash or the wrong branch. A
    // registration Namedrop cannot read or recognize must stop the run with
    // guidance, never fall through to building and linking over it.
    let unusable = [
        (
            r#"{"result":{"plugins":"namedrop"}}"#,
            "how Namedrop is registered",
        ),
        (
            r#"{"result":{"plugins":[{"plugin_id":"namedrop","source":"github"}]}}"#,
            "how Namedrop is registered",
        ),
        (r#"{"result":{}}"#, "how Namedrop is registered"),
        ("not-json", "how Namedrop is registered"),
        // A wrong-typed `plugins` that happens to iterate zero times once read
        // as a genuinely empty registration and authorized build-and-relink.
        (r#"{"result":{"plugins":{}}}"#, "how Namedrop is registered"),
        (r#"{"result":{"plugins":""}}"#, "how Namedrop is registered"),
        (
            r#"{"result":{"plugins":{"namedrop":{"source":{"kind":"github"}}}}}"#,
            "how Namedrop is registered",
        ),
        (r#"{"result":[]}"#, "how Namedrop is registered"),
        ("[]", "how Namedrop is registered"),
        ("null", "how Namedrop is registered"),
        (
            r#"{"result":{"plugins":[{"plugin_id":"namedrop","source":{}}]}}"#,
            "cannot keep (no source kind)",
        ),
        (
            r#"{"result":{"plugins":[{"plugin_id":"namedrop","source":{"kind":"marketplace"}}]}}"#,
            "cannot keep ('marketplace')",
        ),
    ];
    for (response, expected) in unusable {
        let fixture = Fixture::new();
        let checkout = fixture.checkout();
        fixture.host();
        fixture.cargo(0);
        let output = fixture
            .setup(&checkout, None)
            .env("NAMEDROP_TEST_LIST_RESPONSE", response)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{response}: {output:?}");
        let message = stderr(&output);
        assert!(message.contains(expected), "{response}: {message}");
        assert!(
            !message.contains("Traceback"),
            "{response} produced a traceback instead of guidance: {message}"
        );
        // Exiting nonzero is not enough: the run must stop *before* it mutates
        // anything, so neither the build nor the relink may have happened.
        assert!(
            !fixture.cargo_log().exists(),
            "{response} was built anyway: {}",
            fs::read_to_string(fixture.cargo_log()).unwrap_or_default()
        );
        assert!(
            !fixture.calls().contains("plugin\nlink\n"),
            "{response} was relinked anyway: {}",
            fixture.calls()
        );
        assert!(
            !fixture.calls().contains("plugin\naction\n"),
            "{response} still activated Namedrop: {}",
            fixture.calls()
        );
    }
}

#[test]
fn a_replacement_installation_registers_its_own_directory_and_never_the_one_it_replaces() {
    // Maintenance replaces the installation, so the helper must link the
    // directory it was run from — not whatever happens to be registered. Both
    // registrations a replacement can meet are covered: a link left by the
    // previous install, and none at all after an explicit `plugin unlink`.
    for kind in ["local", "none"] {
        let fixture = Fixture::new();
        let previous = fixture.checkout();
        let replacement = fixture.replacement_checkout();
        assert_ne!(previous, replacement);
        fixture.host();
        fixture.cargo(0);

        let output = succeeded(
            &fixture
                .setup(&replacement, None)
                .env("NAMEDROP_TEST_LIST_RESPONSE", registration(kind))
                .output()
                .unwrap(),
        );
        assert!(output.contains("Next steps"), "{kind}: {output}");
        let calls = fixture.calls();
        assert!(
            calls.contains(&format!(
                "plugin\nlink\n{}\n--enabled\n",
                replacement.display()
            )),
            "{kind}: the replacement directory was not the one linked: {calls}"
        );
        assert!(
            !calls.contains(&previous.display().to_string()),
            "{kind}: a host call still named the installation being replaced: {calls}"
        );
        assert!(
            calls.contains("plugin\naction\ninvoke\nactivate\n--plugin\nnamedrop\n"),
            "{kind}: the replacement was linked but never activated: {calls}"
        );
        // State lives outside both directories, so a replacement has no reason
        // to touch either one's contents.
        for checkout in [&previous, &replacement] {
            assert!(
                checkout.join("herdr-plugin.toml").is_file(),
                "{kind}: the replacement disturbed {}",
                checkout.display()
            );
        }
    }
}

#[test]
fn migrating_to_a_managed_installation_activates_it_without_relinking_any_local_directory() {
    // After `herdr plugin unlink` + `herdr plugin install`, the documented last
    // step is `setup.sh activate` from the checkout Herdr now owns. Run from
    // that managed checkout it must activate and nothing else; run in install
    // mode it must recognise the managed origin and refuse to relink, so a
    // migration cannot be undone by the command that finishes it.
    let fixture = Fixture::new();
    let managed = fixture.replacement_checkout();
    fixture.host();
    fixture.cargo(0);

    let activated = succeeded(
        &fixture
            .setup(&managed, Some("activate"))
            .env("NAMEDROP_TEST_LIST_RESPONSE", registration("github"))
            .output()
            .unwrap(),
    );
    assert!(
        activated.contains("/isolated state/namedrop"),
        "activation did not report the state directory the migration preserves: {activated}"
    );
    let calls = fixture.calls();
    assert!(
        !calls.contains("plugin\nlink\n"),
        "explicit activation relinked the managed checkout as a local directory: {calls}"
    );
    assert!(
        !fixture.cargo_log().exists(),
        "explicit activation rebuilt the managed checkout"
    );

    let install_mode = succeeded(
        &fixture
            .setup(&managed, None)
            .env("NAMEDROP_TEST_LIST_RESPONSE", registration("github"))
            .output()
            .unwrap(),
    );
    assert!(
        install_mode.contains("Using the installed GitHub revision"),
        "{install_mode}"
    );
    assert!(
        !fixture.calls().contains("plugin\nlink\n"),
        "install mode converted the migrated installation back into a local link: {}",
        fixture.calls()
    );
    assert!(
        !fixture.cargo_log().exists(),
        "install mode rebuilt the migrated installation"
    );
}
