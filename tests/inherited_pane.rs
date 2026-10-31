#[path = "support/app_popup.rs"]
mod app_popup;
#[path = "support/codex_app_server.rs"]
mod codex_app_server;
use namedrop::{HerdrCommandExecutor, HerdrCommandOutput};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

/// Herdr as seen through the executor port: the pane's shell is whatever pid
/// the test says it is.
struct Shell(Option<u64>);
impl HerdrCommandExecutor for Shell {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        assert_eq!(args, ["pane", "process-info", "--pane", "wH:p1"]);
        let shell_pid = self.0.ok_or_else(|| "pane not found".to_owned())?;
        Ok(HerdrCommandOutput {
            stdout: format!(
                r#"{{"result":{{"process_info":{{"pane_id":"wH:p1","shell_pid":{shell_pid},"foreground_process_group_id":{shell_pid},"foreground_processes":[]}}}}}}"#
            ),
        })
    }
}

/// A live process that is certainly not an ancestor of this one.
fn unrelated_process() -> std::process::Child {
    Command::new("sleep").arg("30").spawn().unwrap()
}

#[test]
fn an_inherited_pane_whose_shell_is_an_ancestor_is_accepted() {
    let parent = u64::from(std::os::unix::process::parent_id());
    assert_eq!(
        namedrop::TuiHost::new(&Shell(Some(parent))).verify_inherited_pane("wH:p1", &[]),
        Ok(())
    );
}

#[test]
fn an_inherited_pane_whose_shell_is_not_an_ancestor_is_refused_with_the_remedy() {
    let mut other = unrelated_process();
    let error = namedrop::TuiHost::new(&Shell(Some(u64::from(other.id()))))
        .verify_inherited_pane("wH:p1", &[])
        .unwrap_err();
    other.kill().unwrap();
    other.wait().unwrap();
    assert!(error.contains("HERDR_PANE_ID=wH:p1"), "{error}");
    assert!(error.contains("not running inside pane wH:p1"), "{error}");
    assert!(error.contains("--herdr-pane"), "{error}");
}

#[test]
fn an_inherited_pane_herdr_cannot_describe_is_refused_with_herdr_s_own_reason() {
    let error = namedrop::TuiHost::new(&Shell(None))
        .verify_inherited_pane("wH:p1", &[])
        .unwrap_err();
    assert!(error.contains("HERDR_PANE_ID=wH:p1"), "{error}");
    assert!(error.contains("pane not found"), "{error}");
    assert!(error.contains("--herdr-pane"), "{error}");
}

/// The binary with a fake `herdr` on `HERDR_BIN_PATH` and a temporary home.
/// `pane process-info` reports the shell pid written to `shell-pid`.
struct Fixture(PathBuf);
impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "namedrop-inherited-pane-{}-{label}",
            std::process::id()
        ));
        fs::create_dir_all(root.join("home")).unwrap();
        let host = root.join("herdr");
        fs::write(
            &host,
            format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> '{root}/herdr-calls'
case "$1 $2" in
  'worktree list') printf '%s\n' '{{"result":{{"worktrees":[{{"path":"/repo","open_workspace_id":"w1"}}]}}}}';;
  'session list') printf '%s\n' '{{"sessions":[]}}';;
  'pane list') printf '%s\n' '{{"result":{{"panes":[{{"pane_id":"p1","terminal_id":"t1","tab_id":"tab1","workspace_id":"w1","agent_status":"unknown","agent_session":{{"value":"session-p1"}}}},{{"pane_id":"p2","terminal_id":"t2","tab_id":"tab1","workspace_id":"w1","agent_status":"unknown","agent_session":{{"value":"session-p2"}}}}]}}}}';;
  'pane process-info') printf '{{"result":{{"process_info":{{"pane_id":"p1","shell_pid":%s,"foreground_process_group_id":42,"foreground_processes":[]}}}}}}\n' "$(cat '{root}/shell-pid')";;
  'server reload-config'|'pane report-metadata'|'tab rename') printf '%s\n' '{{}}';;
  'tab get') printf '%s\n' '{{"result":{{"label":"Main"}}}}';;
  *) printf 'unexpected Herdr call: %s\n' "$*" >&2; exit 1;;
esac
"#,
                root = root.display()
            ),
        )
        .unwrap();
        fs::set_permissions(host, fs::Permissions::from_mode(0o700)).unwrap();
        app_popup::fake(&root);
        Self(root)
    }
    /// The test process is the parent of every binary it runs, so its pid is
    /// an ancestor of all of them.
    fn shell_is_an_ancestor(&self) {
        fs::write(self.0.join("shell-pid"), std::process::id().to_string()).unwrap();
    }
    fn shell_is(&self, pid: u32) {
        fs::write(self.0.join("shell-pid"), pid.to_string()).unwrap();
    }
    fn namedrop(&self, args: &[&str]) -> std::process::Output {
        self.namedrop_command(args).output().unwrap()
    }
    fn namedrop_command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_namedrop"));
        self.host(&mut command)
            .env_remove("HERDR_PLUGIN_STATE_DIR")
            .env("PATH", "/usr/bin:/bin")
            .arg("--plugin-state")
            .arg(self.0.join("state"))
            .args(args);
        command
    }
    /// Herdr's `activate` action: `scripts/run.sh activate` from a checkout
    /// with the built binary staged, and `herdr` on PATH for the scripts.
    fn activate_action(&self) -> std::process::Output {
        let checkout = self.0.join("checkout");
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        fs::create_dir_all(checkout.join("scripts")).unwrap();
        fs::create_dir_all(checkout.join("bin")).unwrap();
        for script in ["run.sh", "runtime.py", "update.py"] {
            fs::copy(
                repo.join("scripts").join(script),
                checkout.join("scripts").join(script),
            )
            .unwrap();
        }
        fs::copy(repo.join("runtime.json"), checkout.join("runtime.json")).unwrap();
        fs::copy(
            env!("CARGO_BIN_EXE_namedrop"),
            checkout.join("bin/namedrop"),
        )
        .unwrap();
        fs::create_dir_all(self.0.join("path")).unwrap();
        std::os::unix::fs::symlink(self.0.join("herdr"), self.0.join("path/herdr")).unwrap();
        fs::create_dir_all(self.0.join("state")).unwrap();
        self.host(Command::new("sh").arg(checkout.join("scripts/run.sh")))
            .env("HERDR_PLUGIN_STATE_DIR", self.0.join("state"))
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.0.join("path").display()),
            )
            .args(["activate", "--input-json", "{\"confirmed\":true}"])
            .output()
            .unwrap()
    }
    fn host<'a>(&self, command: &'a mut Command) -> &'a mut Command {
        command
            .env("HOME", self.0.join("home"))
            .env("CLAUDE_CONFIG_DIR", self.0.join("home").join(".claude"))
            .env("CODEX_HOME", self.0.join("home").join(".codex"))
            .env_remove("XDG_CONFIG_HOME")
            .env("CODEX_HOME", self.0.join("home/.codex"))
            .env_remove("ZDOTDIR")
            .env_remove("CODEX_SESSION_ID")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("HERDR_CONFIG_PATH")
            .env("SHELL", "/bin/zsh")
            .env("HERDR_APP_BIN_PATH", self.0.join("app-popup"))
            .env("HERDR_BIN_PATH", self.0.join("herdr"))
            .env("HERDR_SOCKET_PATH", "isolated-socket")
            .env("HERDR_WORKSPACE_ID", "w1")
            .env("HERDR_PANE_ID", "p1")
    }
    fn activate(&self) {
        self.shell_is_an_ancestor();
        let output = self.namedrop(&["activate", "--input-json", "{\"confirmed\":true}"]);
        assert!(output.status.success(), "{output:?}");
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn text(output: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn the_binary_refuses_an_inherited_pane_it_does_not_run_inside() {
    let f = Fixture::new("refused");
    f.activate();
    let mut other = unrelated_process();
    f.shell_is(other.id());
    let output = f.namedrop(&["address", "list"]);
    other.kill().unwrap();
    other.wait().unwrap();
    assert!(!output.status.success(), "{output:?}");
    let text = text(&output);
    assert!(text.contains("HERDR_PANE_ID=p1"), "{text}");
    assert!(text.contains("--herdr-pane"), "{text}");
    assert!(
        !text.contains("\"addresses\""),
        "no roster for a pane this process is not in: {text}"
    );
}

#[test]
fn the_binary_accepts_an_inherited_pane_it_runs_inside() {
    let f = Fixture::new("accepted");
    f.activate();
    let output = f.namedrop(&["address", "list"]);
    assert!(output.status.success(), "{output:?}");
    let listed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(listed["status"], "ok", "{listed}");
}

#[test]
fn an_explicit_pane_is_the_callers_assertion_and_is_not_checked() {
    let f = Fixture::new("explicit");
    f.activate();
    let mut other = unrelated_process();
    f.shell_is(other.id());
    let output = f.namedrop(&["--herdr-pane", "p1", "address", "list"]);
    other.kill().unwrap();
    other.wait().unwrap();
    assert!(output.status.success(), "{output:?}");
    let listed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(listed["status"], "ok", "{listed}");
}

#[test]
fn herdr_s_activate_action_passes_its_pane_to_activation() {
    // Herdr runs the action as its own child, so the pane's shell is never an
    // ancestor of the runtime that activation starts.
    let f = Fixture::new("action");
    let mut other = unrelated_process();
    f.shell_is(other.id());
    let output = f.activate_action();
    other.kill().unwrap();
    other.wait().unwrap();
    let text = text(&output);
    assert!(output.status.success(), "{text}");
    let activated: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(activated["data"]["activated"], true, "{text}");
}

#[test]
fn an_inherited_pane_is_accepted_when_the_agent_session_runs_in_it() {
    // Claude Code's Bash can run outside the pane shell's process tree, yet
    // its session ID still names the pane Herdr says runs it.
    let f = Fixture::new("session");
    f.activate();
    let mut other = unrelated_process();
    f.shell_is(other.id());
    let output = f
        .namedrop_command(&["address", "list"])
        .env("CLAUDE_CODE_SESSION_ID", "session-p1")
        .output()
        .unwrap();
    other.kill().unwrap();
    other.wait().unwrap();
    assert!(output.status.success(), "{}", text(&output));
    let listed: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(listed["status"], "ok", "{listed}");
}

#[test]
fn an_agent_session_running_in_another_pane_does_not_vouch_for_the_inherited_pane() {
    let f = Fixture::new("session-elsewhere");
    f.activate();
    let mut other = unrelated_process();
    f.shell_is(other.id());
    let output = f
        .namedrop_command(&["address", "list"])
        .env("CLAUDE_CODE_SESSION_ID", "session-p2")
        .output()
        .unwrap();
    other.kill().unwrap();
    other.wait().unwrap();
    assert!(!output.status.success(), "{}", text(&output));
    let text = text(&output);
    assert!(text.contains("HERDR_PANE_ID=p1"), "{text}");
    assert!(text.contains("--herdr-pane"), "{text}");
}

#[test]
fn a_stale_codex_session_does_not_hide_the_claude_session_that_runs_in_the_pane() {
    let f = Fixture::new("two-sessions");
    f.activate();
    let mut other = unrelated_process();
    f.shell_is(other.id());
    let output = f
        .namedrop_command(&["address", "list"])
        .env("CODEX_SESSION_ID", "session-from-an-outer-codex")
        .env("CLAUDE_CODE_SESSION_ID", "session-p1")
        .output()
        .unwrap();
    other.kill().unwrap();
    other.wait().unwrap();
    assert!(output.status.success(), "{}", text(&output));
}

/// Herdr's own `pane list` output, captured from a live pane: many more
/// fields than the fixture's, with the session under `agent_session`.
struct LivePaneList;
impl HerdrCommandExecutor for LivePaneList {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let stdout = match args {
            ["pane", "process-info", ..] => {
                r#"{"id":"cli:pane:process_info","result":{"process_info":{"foreground_process_group_id":1147346,"foreground_processes":[{"argv":["claude"],"cmdline":"claude","cwd":"/repo","name":"claude","pid":1147346}],"pane_id":"wG:p21","shell_pid":4000000000},"type":"pane_process_info"}}"#
            }
            ["pane", "list"] => {
                r#"{"id":"cli:pane:list","result":{"panes":[{"agent":"claude","agent_session":{"agent":"claude","kind":"id","source":"herdr:claude","value":"sess-21"},"agent_status":"working","cwd":"/repo","focused":false,"foreground_cwd":"/repo","pane_id":"wG:p21","revision":20,"scroll":{"max_offset_from_bottom":0,"offset_from_bottom":0,"viewport_rows":40},"state_labels":{"working":"@a working"},"tab_id":"wG:tP","terminal_id":"term_1","terminal_title":"t","title":"@a","tokens":{"x":"0"},"workspace_id":"wG"},{"agent":"claude","agent_session":{"agent":"claude","kind":"id","source":"herdr:claude","value":"sess-1"},"agent_status":"idle","cwd":"/repo","pane_id":"wG:p1","tab_id":"wG:tP","terminal_id":"term_2","workspace_id":"wG"},{"agent_status":"unknown","pane_id":"w9:p1","tab_id":"w9:t1","terminal_id":"term_3","workspace_id":"w9"}],"type":"pane_list"}}"#
            }
            other => return Err(format!("unexpected call {other:?}")),
        };
        Ok(HerdrCommandOutput {
            stdout: stdout.to_owned(),
        })
    }
}

#[test]
fn the_live_pane_list_shape_vouches_for_the_session_s_own_pane_only() {
    assert_eq!(
        namedrop::TuiHost::new(&LivePaneList).verify_inherited_pane("wG:p21", &["sess-21"]),
        Ok(())
    );
    let error = namedrop::TuiHost::new(&LivePaneList)
        .verify_inherited_pane("wG:p21", &["sess-1"])
        .unwrap_err();
    assert!(error.contains("--herdr-pane"), "{error}");
    let error = namedrop::TuiHost::new(&LivePaneList)
        .verify_inherited_pane("wG:p21", &["nobody"])
        .unwrap_err();
    assert!(error.contains("--herdr-pane"), "{error}");
}

#[test]
fn a_session_vouches_for_the_pane_when_the_process_is_a_child_of_init() {
    // A double fork (a backgrounded subshell whose parent exits) reparents the
    // command to init, so no ancestor of it is the pane's shell. Unlike
    // `setsid`, it works on macOS.
    let f = Fixture::new("orphan");
    f.activate();
    let mut other = unrelated_process();
    f.shell_is(other.id());
    let out = f.0.join("orphan.out");
    let mut command = f.namedrop_command(&["address", "list"]);
    command.env("CLAUDE_CODE_SESSION_ID", "session-p1");
    let mut detached = Command::new("sh");
    detached
        .arg("-c")
        .arg(r#"( "$0" "$@" > "$OUT" 2>&1 & )"#)
        .arg(command.get_program())
        .args(command.get_args())
        .env("OUT", &out);
    for (key, value) in command.get_envs() {
        match value {
            Some(value) => detached.env(key, value),
            None => detached.env_remove(key),
        };
    }
    assert!(detached.status().unwrap().success());
    let mut waited = 0;
    while fs::read_to_string(&out).unwrap_or_default().is_empty() && waited < 100 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        waited += 1;
    }
    other.kill().unwrap();
    other.wait().unwrap();
    let printed = fs::read_to_string(&out).unwrap_or_default();
    assert!(printed.contains("\"status\":\"ok\""), "{printed}");
}

#[test]
fn the_mailbox_wake_hook_drains_the_space_after_its_hook_shell_has_exited() {
    // The user-prompt-submit wrapper backgrounds the wake and exits, so the
    // wake runs reparented away from the pane's shell. Codex gives its hooks
    // no session ID that could vouch for the inherited pane, and the wake
    // needs none: it reconciles the whole space, as nobody's pane.
    let f = Fixture::new("wake");
    f.activate();
    let mut other = unrelated_process();
    f.shell_is(other.id());
    let calls = f.0.join("herdr-calls");
    fs::write(&calls, "").unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_namedrop"));
    f.host(&mut command)
        .env_remove("HERDR_PLUGIN_STATE_DIR")
        .env("PATH", "/usr/bin:/bin")
        .arg("hook")
        .arg("mailbox-wake")
        .arg("--state-directory")
        .arg(f.0.join("state"))
        .stdin(std::process::Stdio::null());
    let mut hook = command.spawn().unwrap();
    let drained = |calls: &Path| {
        fs::read_to_string(calls)
            .unwrap_or_default()
            .lines()
            .any(|call| call.starts_with("pane list"))
    };
    let mut waited = 0;
    while !drained(&calls) && hook.try_wait().unwrap().is_none() && waited < 200 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        waited += 1;
    }
    let _ = hook.kill();
    let _ = hook.wait();
    other.kill().unwrap();
    other.wait().unwrap();
    assert!(
        drained(&calls),
        "the wake never asked Herdr for the space's panes, so nothing was drained; Herdr calls: {:?}",
        fs::read_to_string(&calls).unwrap_or_default()
    );
}

#[test]
fn a_refusal_says_how_many_agent_sessions_it_checked() {
    let none = namedrop::TuiHost::new(&LivePaneList)
        .verify_inherited_pane("wG:p21", &[])
        .unwrap_err();
    assert!(
        none.contains("There is no CODEX_SESSION_ID or CLAUDE_CODE_SESSION_ID in the environment"),
        "{none}"
    );
    let elsewhere = namedrop::TuiHost::new(&LivePaneList)
        .verify_inherited_pane("wG:p21", &["sess-1"])
        .unwrap_err();
    assert!(
        elsewhere
            .contains("None of the 1 agent session ID(s) in the environment runs in pane wG:p21"),
        "{elsewhere}"
    );
}

#[test]
fn an_empty_session_variable_is_not_counted_as_a_session() {
    let f = Fixture::new("empty-session");
    f.activate();
    let mut other = unrelated_process();
    f.shell_is(other.id());
    let output = f
        .namedrop_command(&["address", "list"])
        .env("CLAUDE_CODE_SESSION_ID", "")
        .output()
        .unwrap();
    other.kill().unwrap();
    other.wait().unwrap();
    assert!(!output.status.success(), "{}", text(&output));
    let text = text(&output);
    assert!(text.contains("There is no CODEX_SESSION_ID"), "{text}");
    assert!(!text.contains("None of the 1"), "{text}");
}

#[test]
fn a_session_start_inside_codex_s_shared_app_server_gets_its_notice_without_a_runtime() {
    let f = Fixture::new("app-server");
    f.activate();
    let other = codex_app_server::Process::spawn(Command::new("sleep").arg("60"), "sleep");
    f.shell_is(other.pid());
    let codex_home = f.0.join("home/.codex");
    let hook = |event: &str, input: &str, inside: bool| {
        let program = Path::new(env!("CARGO_BIN_EXE_namedrop"));
        let mut command = if inside {
            codex_app_server::inside_server(&codex_home, program)
        } else {
            Command::new(program)
        };
        f.host(&mut command)
            .env_remove("HERDR_PLUGIN_STATE_DIR")
            .args(["hook", event, "--state-directory"])
            .arg(f.0.join("state"));
        let output = codex_app_server::run_with_stdin(&mut command, input);
        assert!(output.status.success(), "{output:?}");
        String::from_utf8(output.stdout).unwrap()
    };
    let start =
        r#"{"hook_event_name":"SessionStart","source":"startup","session_id":"s1","cwd":"/repo"}"#;

    let notice = hook("session-start", start, true);
    assert!(notice.ends_with('}'), "no trailing newline: {notice:?}");
    let value: serde_json::Value = serde_json::from_str(&notice).expect(&notice);
    assert_eq!(
        value.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["hookSpecificOutput", "systemMessage"]
    );
    assert_eq!(value["hookSpecificOutput"]["hookEventName"], "SessionStart");
    let banner = value["systemMessage"].as_str().unwrap();
    let context = value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(!banner.contains('\n'), "one warning line: {banner}");
    assert!(context.contains(codex_app_server::SUMMARY), "{context}");
    assert!(
        context.contains("without the person's approval"),
        "{context}"
    );
    for command in codex_app_server::COMMANDS {
        assert!(banner.contains(command), "{command}: {banner}");
        assert!(
            context.lines().any(|line| line == command),
            "{command} on its own line: {context}"
        );
    }
    for source in ["resume", "fork"] {
        let input = start.replace("startup", source);
        assert_eq!(hook("session-start", &input, true), notice, "{source}");
    }

    assert_eq!(
        hook("session-start", start, false),
        "",
        "outside the server"
    );
    for input in [
        start.replace("startup", "clear"),
        start.replace("startup", "compact"),
        "not json".to_owned(),
        start.replace("SessionStart", "UserPromptSubmit"),
    ] {
        assert_eq!(hook("session-start", &input, true), "", "{input}");
    }
    let prompt = r#"{"hook_event_name":"UserPromptSubmit","prompt":"hello","session_id":"s1"}"#;
    assert_eq!(hook("user-prompt-submit", prompt, true), "");
}

#[test]
fn hc_binary_verifies_inherited_identity() {
    let f = Fixture::new("hc");
    let hc = f.0.join("hc");
    let record = f.0.join("hc-terminal.json");
    let mut terminal: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/hc/get-labeled.stdout")).unwrap();
    terminal["pid"] = serde_json::json!(4_000_000_000u32);
    fs::write(&record, terminal.to_string()).unwrap();
    fs::write(&hc, format!(r#"#!/bin/sh
case "$1" in
  get) cat '{}' ;;
  ls) printf '{{"terminals":[{{"address":"t1","state":"live"}}]}}' ;;
  server) printf '{{"core":{{"id":"test-core"}}}}' ;;
  send) test -z "${{HC_TERMINAL_ID+x}}" && test -z "${{HC_TERMINAL_SOCKET+x}}" && test "$HC_SESSION" = fixture-session && test "$HC_SOCKET" = fixture-socket || exit 1; printf '{{}}' ;;
  *) exit 1 ;;
esac
"#, record.display())).unwrap();
    fs::set_permissions(&hc, fs::Permissions::from_mode(0o755)).unwrap();
    let run = |session: Option<&str>| {
        let mut command = f.namedrop_command(&[
            "--host",
            "hc",
            "--hc-space",
            "fixture-space",
            "address",
            "list",
        ]);
        command.env("HC_BIN_PATH", &hc).env("HC_TERMINAL_ID", "t1");
        if let Some(session) = session {
            command.env("CLAUDE_CODE_SESSION_ID", session);
        }
        command.output().unwrap()
    };
    let refused = text(&run(None));
    assert!(refused.contains("HC_TERMINAL_ID is stale"), "{refused}");
    let recovered = text(&run(Some("fixture-session")));
    assert!(
        !recovered.contains("HC_TERMINAL_ID is stale"),
        "{recovered}"
    );
    assert!(!recovered.contains("unexpected Herdr call"), "{recovered}");
    terminal["pid"] = serde_json::json!(std::process::id());
    fs::write(&record, terminal.to_string()).unwrap();
    assert!(!text(&run(None)).contains("HC_TERMINAL_ID is stale"));
}

#[test]
fn hc_hooks_write_sessions_only_in_active_spaces_and_preserve_core_routing() {
    let f = Fixture::new("hc-hooks");
    f.activate();
    let hc_state = f.0.join("hc-state");
    let tui_packages = [
        namedrop::AgentRuntime::ClaudeCode,
        namedrop::AgentRuntime::Codex,
    ]
    .map(|agent| {
        let path = namedrop::package_directory(&f.0.join("home"), agent);
        let snapshot = integration_snapshot(&path);
        (path, snapshot)
    });
    let launcher = f.0.join("home/.local/bin/namedrop");
    let tui_fallback = fs::read_to_string(&launcher).unwrap();
    let tui_rules = fs::read(f.0.join("home/.codex/rules/namedrop.rules")).unwrap();

    let hc = f.0.join("hc");
    let record = f.0.join("record.json");
    let mut terminal: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/hc/get-labeled.stdout")).unwrap();
    terminal["pid"] = serde_json::json!(std::process::id());
    terminal["labels"]
        .as_object_mut()
        .unwrap()
        .remove("namedrop.session");
    terminal["labels"]
        .as_object_mut()
        .unwrap()
        .remove("namedrop.name");
    terminal["status"] = serde_json::json!({"state":"idle","app":"claude-code"});
    terminal["reported"] = serde_json::json!(true);
    fs::write(&record, terminal.to_string()).unwrap();
    fs::write(
        &hc,
        r#"#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
root = Path(__file__).parent
args = sys.argv[1:]
assert 'HC_TERMINAL_ID' not in os.environ
assert 'HC_TERMINAL_SOCKET' not in os.environ
assert os.environ['HC_SESSION'] in ('hook-core', 'other-hook-core')
assert os.environ['HC_SOCKET'] == 'hook-socket'
with (root / 'hc-calls').open('a') as out:
    out.write(json.dumps(args) + '\n')
record_path = root / ('second-record.json' if len(args) > 1 and args[1] == 't2' else 'record.json')
record = json.loads(record_path.read_text())
if args[0] == '--version': print('herdr-core 0.1.14')
elif args[0] == 'get': print(json.dumps(record))
elif args[0] == 'ls':
    records = [record]
    if (root / 'second-record.json').exists(): records.append(json.loads((root / 'second-record.json').read_text()))
    print(json.dumps({'terminals':records}))
elif args[0] == 'server': print(json.dumps({'core':{'id':'hook-test'}}))
elif args[0] == 'label':
    for arg in args[2:]:
        if arg == '--json': continue
        key, value = arg.split('=', 1)
        if value: record['labels'][key] = value
        else: record['labels'].pop(key, None)
    record_path.write_text(json.dumps(record))
    print(json.dumps(record))
elif args[0] == 'run':
    print(json.dumps({'error':{'kind':'name_taken','message':'already running'}}), file=sys.stderr)
    sys.exit(1)
elif args[0] == 'send':
    assert 'HC_TERMINAL_ID' not in os.environ
    assert 'HC_TERMINAL_SOCKET' not in os.environ
    assert os.environ['HC_SESSION'] in ('hook-core', 'other-hook-core')
    assert os.environ['HC_SOCKET'] == 'hook-socket'
    print(json.dumps({'started':True}))
else: raise AssertionError(args)
"#,
    )
    .unwrap();
    fs::set_permissions(&hc, fs::Permissions::from_mode(0o755)).unwrap();
    let command = |args: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_namedrop"));
        f.host(&mut command)
            .arg("--plugin-state")
            .arg(&hc_state)
            .args(args);
        command
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_PANE_ID")
            .env("HC_BIN_PATH", &hc)
            .env("HC_TERMINAL_ID", "t1")
            .env("HC_TERMINAL_SOCKET", "inherited-terminal-socket")
            .env("HC_SESSION", "hook-core")
            .env("HC_SOCKET", "hook-socket");
        command
    };
    let run_hook = || {
        codex_app_server::run_with_stdin(
            &mut command(&[
                "--host",
                "hc",
                "--hc-hook",
                "--hc-terminal",
                "t1",
                "--hc-space",
                "fixture-space",
                "hook",
                "session-start",
                "--state-directory",
                hc_state.clone().to_str().unwrap(),
            ]),
            r#"{"hook_event_name":"SessionStart","session_id":"real-hook-session","source":"startup"}"#,
        )
    };
    assert!(run_hook().stdout.is_empty());
    let before: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&record).unwrap()).unwrap();
    assert!(before["labels"]["namedrop.session"].is_null());
    let namespace = namedrop::StateNamespace::for_space("hc:hook-test", "fixture-space");
    namedrop::StateStore::open(hc_state.clone())
        .unwrap()
        .transaction(|transaction| {
            transaction.activate_worktree(
                &namespace,
                &namedrop::MutationAttribution::new(namedrop::ActorKind::Human, "test", 1),
            )
        })
        .unwrap();
    let active = run_hook();
    assert!(active.status.success(), "{}", text(&active));
    assert!(
        !active.stdout.is_empty(),
        "active hook should supply the session policy"
    );
    let after: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&record).unwrap()).unwrap();
    assert_eq!(after["labels"]["namedrop.session"], "real-hook-session");
    run_hook();
    let calls = fs::read_to_string(f.0.join("hc-calls")).unwrap();
    assert_eq!(
        calls
            .lines()
            .filter(|line| line.starts_with("[\"label\""))
            .count(),
        1,
        "session writes are idempotent"
    );
    assert_eq!(
        calls
            .lines()
            .filter(|line| line.starts_with("[\"run\""))
            .count(),
        2
    );
    let named = command(&[
        "--host",
        "hc",
        "--hc-terminal",
        "t1",
        "--hc-space",
        "fixture-space",
        "name",
        "hook-agent",
    ])
    .output()
    .unwrap();
    assert!(named.status.success(), "{}", text(&named));
    let result: serde_json::Value = serde_json::from_slice(&named.stdout).unwrap();
    for runtime in ["claude-code", "codex"] {
        let package = Path::new(
            result["data"]["integration_packages"][runtime]["path"]
                .as_str()
                .unwrap(),
        );
        let hooks: serde_json::Value =
            serde_json::from_slice(&fs::read(package.join("hooks/hooks.json")).unwrap()).unwrap();
        for event in ["SessionStart", "UserPromptSubmit"] {
            let timeout = hooks["hooks"][event][0]["hooks"][0]["timeout"]
                .as_f64()
                .unwrap();
            assert!(
                timeout > 10.2 && timeout <= 60.0,
                "HC {runtime} {event} framework timeout {timeout} can kill an in-flight send"
            );
        }
    }

    assert_eq!(
        result["data"]["registered"], true,
        "the ordinary native naming path registers this session"
    );
    let doctor = command(&[
        "--host",
        "hc",
        "--hc-terminal",
        "t1",
        "--hc-space",
        "fixture-space",
        "doctor",
    ])
    .output()
    .unwrap();
    let diagnosis: serde_json::Value = serde_json::from_slice(&doctor.stdout).unwrap();
    assert_eq!(diagnosis["data"]["host"], "herdr-core");
    assert_eq!(diagnosis["data"]["host_version"], "herdr-core 0.1.14");
    for (path, before) in &tui_packages {
        assert_eq!(
            &integration_snapshot(path),
            before,
            "HC reconcile changed TUI bytes"
        );
    }
    assert_eq!(
        fs::read(f.0.join("home/.codex/rules/namedrop.rules")).unwrap(),
        tui_rules
    );
    let tui_doctor = f.namedrop(&["doctor"]);
    let diagnosis: serde_json::Value = serde_json::from_slice(&tui_doctor.stdout).unwrap();
    assert!(
        !diagnosis.to_string().contains("ND-INTEGRATION-001"),
        "{}",
        text(&tui_doctor)
    );
    let hc_package = Path::new(
        result["data"]["integration_packages"]["codex"]["path"]
            .as_str()
            .unwrap(),
    );
    let hc_snapshot = integration_snapshot(hc_package);
    let foreign_state = f.0.join("foreign-hc");
    let mut foreign = Command::new(env!("CARGO_BIN_EXE_namedrop"));
    f.host(&mut foreign)
        .env("HC_BIN_PATH", &hc)
        .env("HC_SESSION", "other-hook-core")
        .env("HC_SOCKET", "hook-socket")
        .arg("--plugin-state")
        .arg(&foreign_state)
        .args([
            "--host",
            "hc",
            "--hc-space",
            "fixture-space",
            "activate",
            "--input-json",
            "{\"confirmed\":true}",
        ]);
    let foreign = foreign.output().unwrap();
    assert!(foreign.status.success(), "{}", text(&foreign));
    let foreign: serde_json::Value = serde_json::from_slice(&foreign.stdout).unwrap();
    assert_eq!(
        integration_snapshot(hc_package),
        hc_snapshot,
        "another session changed the shared HC package"
    );
    assert_eq!(fs::read_to_string(&launcher).unwrap(), tui_fallback);
    let mut fallback = Command::new(&launcher);
    f.host(&mut fallback)
        .env_remove("HERDR_SOCKET_PATH")
        .env_remove("HERDR_PANE_ID")
        .env_remove("HERDR_WORKSPACE_ID")
        .env_remove("HC_TERMINAL_ID")
        .env_remove("NAMEDROP_HOST")
        .args([
            "--herdr-socket",
            "isolated-socket",
            "--herdr-workspace",
            "w1",
            "--herdr-pane",
            "p1",
            "address",
            "list",
        ]);
    let fallback = fallback.output().unwrap();
    assert!(fallback.status.success(), "{}", text(&fallback));
    let inherited_hook = |program: &Path, args: &[&str], payload: &str| {
        let mut command = Command::new(program);
        command
            .env("HOME", f.0.join("home"))
            .env("CLAUDE_CONFIG_DIR", f.0.join("home/.claude"))
            .env("CODEX_HOME", f.0.join("home/.codex"))
            .args(args)
            .env("HERDR_APP_BIN_PATH", f.0.join("app-popup"))
            .env("HC_BIN_PATH", &hc)
            .env("HC_TERMINAL_ID", "t1")
            .env("HC_SESSION", "hook-core")
            .env("HC_SOCKET", "hook-socket")
            .env("NAMEDROP_STATE_DIR", hc_state.clone())
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_PANE_ID")
            .env_remove("CODEX_SESSION_ID")
            .env_remove("CLAUDE_CODE_SESSION_ID");
        codex_app_server::run_with_stdin(&mut command, payload)
    };
    for runtime in ["claude-code", "codex"] {
        let active_package = Path::new(
            result["data"]["integration_packages"][runtime]["path"]
                .as_str()
                .unwrap(),
        );
        let foreign_package = Path::new(
            foreign["data"]["integration_packages"][runtime]["path"]
                .as_str()
                .unwrap(),
        );
        let tui_package = &tui_packages[usize::from(runtime == "codex")].0;
        assert_eq!(
            active_package, foreign_package,
            "HC sessions share one package"
        );
        for order in [
            [tui_package.as_path(), active_package],
            [active_package, tui_package.as_path()],
        ] {
            for event in ["session-start", "user-prompt-submit", "mailbox-wake"] {
                fs::write(f.0.join("hc-calls"), "").unwrap();
                for package in order {
                    let calls_before = fs::read_to_string(f.0.join("hc-calls")).unwrap();
                    let guards = [
                        f.0.join("state/runtime/namedrop"),
                        hc_state.join("runtime/namedrop"),
                    ];
                    if package != active_package {
                        for guard in &guards {
                            fs::create_dir_all(guard.parent().unwrap()).unwrap();
                            fs::write(guard, "#!/bin/sh\nexit 97\n").unwrap();
                            fs::set_permissions(guard, fs::Permissions::from_mode(0o700)).unwrap();
                        }
                    }

                    let output = if event == "mailbox-wake" {
                        let state = if package == tui_package {
                            f.0.join("state")
                        } else if package == foreign_package {
                            foreign_state.clone()
                        } else {
                            hc_state.clone()
                        };
                        let mut args = vec!["--host".to_owned(), "hc".to_owned()];
                        if package != tui_package {
                            args.push("--hc-hook".into());
                        }
                        args.extend([
                            "hook".into(),
                            event.into(),
                            "--state-directory".into(),
                            state.to_string_lossy().into_owned(),
                        ]);
                        inherited_hook(
                            Path::new(env!("CARGO_BIN_EXE_namedrop")),
                            &args.iter().map(String::as_str).collect::<Vec<_>>(),
                            "",
                        )
                    } else {
                        let payload = if event == "session-start" {
                            r#"{"hook_event_name":"SessionStart","session_id":"real-hook-session","source":"startup"}"#
                        } else {
                            r#"{"hook_event_name":"UserPromptSubmit","prompt":"ordinary local prompt"}"#
                        };
                        inherited_hook(&package.join("scripts").join(event), &[], payload)
                    };
                    if package != active_package {
                        for guard in &guards {
                            fs::remove_file(guard).unwrap();
                        }
                    }
                    assert!(output.status.success(), "{event}: {}", text(&output));
                    if package != active_package {
                        assert!(output.stdout.is_empty(), "foreign package emitted {event}");
                        assert_eq!(
                            fs::read_to_string(f.0.join("hc-calls")).unwrap(),
                            calls_before,
                            "foreign package dispatched {event}"
                        );
                    }
                }
                let calls = fs::read_to_string(f.0.join("hc-calls")).unwrap();
                if event == "session-start" {
                    assert_eq!(
                        calls
                            .lines()
                            .filter(|line| line.starts_with("[\"run\""))
                            .count(),
                        1,
                        "one current package must dispatch SessionStart: {calls}"
                    );
                }
                assert!(
                    calls.contains("[\"server\""),
                    "current {event} must run: {calls}"
                );
            }
        }
        let output = Command::new(active_package.join("scripts/session-start"))
            .env("HERDR_APP_BIN_PATH", f.0.join("app-popup"))
            .env("HERDR_SOCKET_PATH", "tui")
            .env_remove("HC_TERMINAL_ID")
            .output()
            .unwrap();
        assert!(output.stdout.is_empty());
    }
    let mut recipient: serde_json::Value =
        serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    recipient["address"] = serde_json::json!("t2");
    recipient["labels"]["namedrop.session"] = serde_json::json!("recipient-session");
    recipient["labels"]
        .as_object_mut()
        .unwrap()
        .remove("namedrop.name");
    fs::write(f.0.join("second-record.json"), recipient.to_string()).unwrap();
    let recipient = command(&[
        "--host",
        "hc",
        "--hc-terminal",
        "t2",
        "--hc-space",
        "fixture-space",
        "name",
        "target-agent",
    ])
    .output()
    .unwrap();
    assert!(recipient.status.success(), "{}", text(&recipient));
    let db = rusqlite::Connection::open(hc_state.join("namedrop.sqlite3")).unwrap();
    let count = || {
        db.query_row("SELECT COUNT(*) FROM collaboration_messages", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap()
    };
    for order in [[0, 1], [1, 0]] {
        let before = count();
        for index in order {
            let mut args = vec!["--host".to_owned(), "hc".to_owned()];
            if index != 0 {
                args.push("--hc-hook".into());
            }
            args.extend([
                "hook".into(),
                "user-prompt-submit".into(),
                "--state-directory".into(),
                f.0.join("state").to_string_lossy().into_owned(),
            ]);
            let output = inherited_hook(
                Path::new(env!("CARGO_BIN_EXE_namedrop")),
                &args.iter().map(String::as_str).collect::<Vec<_>>(),
                r#"{"hook_event_name":"UserPromptSubmit","prompt":"@target-agent one owner review mention"}"#,
            );
            assert!(output.status.success(), "{}", text(&output));
            if index == 1 {
                assert!(
                    String::from_utf8_lossy(&output.stdout).contains("Collaboration message sent."),
                    "{}",
                    text(&output)
                );
            } else {
                assert!(output.stdout.is_empty());
            }
        }
        assert_eq!(
            count(),
            before + 1,
            "co-loaded packages must persist one actual mention"
        );
    }
    let package = result["data"]["integration_packages"]["claude-code"]["path"]
        .as_str()
        .unwrap();
    let mut stale: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&record).unwrap()).unwrap();
    let mut unrelated = unrelated_process();
    stale["pid"] = serde_json::json!(unrelated.id());
    stale["labels"]
        .as_object_mut()
        .unwrap()
        .remove("namedrop.session");
    fs::write(&record, stale.to_string()).unwrap();
    fs::write(f.0.join("hc-calls"), "").unwrap();
    let mut wrapper = Command::new(Path::new(package).join("scripts/session-start"));
    wrapper
        .env("HOME", f.0.join("home"))
        .env("CLAUDE_CONFIG_DIR", f.0.join("home/.claude"))
        .env("CODEX_HOME", f.0.join("home/.codex"))
        .env("HERDR_APP_BIN_PATH", f.0.join("app-popup"))
        .env("HC_BIN_PATH", &hc)
        .env("HC_TERMINAL_ID", "t1")
        .env("HC_SESSION", "hook-core")
        .env("HC_SOCKET", "hook-socket")
        .env("NAMEDROP_STATE_DIR", hc_state.clone())
        .env_remove("HERDR_SOCKET_PATH")
        .env_remove("HERDR_PANE_ID")
        .env_remove("CODEX_SESSION_ID")
        .env_remove("CLAUDE_CODE_SESSION_ID");
    let stale_output = codex_app_server::run_with_stdin(
        &mut wrapper,
        r#"{"hook_event_name":"SessionStart","session_id":"stale-hook-session","source":"startup"}"#,
    );
    unrelated.kill().unwrap();
    unrelated.wait().unwrap();
    assert!(
        stale_output.stdout.is_empty(),
        "stale inherited terminal must be declined"
    );
    let calls = fs::read_to_string(f.0.join("hc-calls")).unwrap();
    assert!(
        !calls.lines().any(|line| line.starts_with("[\"label\"")),
        "stale inherited terminal wrote a label: {calls}"
    );
    let selected_runtime = hc_state.join("runtime/namedrop");
    fs::create_dir_all(selected_runtime.parent().unwrap()).unwrap();
    fs::write(
        &selected_runtime,
        "#!/bin/sh\nprintf '%s\\n' selected-runtime \"$@\"\n",
    )
    .unwrap();
    fs::set_permissions(&selected_runtime, fs::Permissions::from_mode(0o755)).unwrap();
    let output = inherited_hook(
        &f.0.join("home/.local/bin/namedrop-app"),
        &["doctor", "--host", "hc"],
        "",
    );
    assert!(output.status.success(), "{}", text(&output));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "selected-runtime\ndoctor\n--host\nhc\n"
    );
    app_popup::assert_tour(&f.0, 1);
}

#[test]
fn explicit_transport_flags_select_the_host_without_environment() {
    let fixture = Fixture::new("explicit-host-flags");
    let herdr = fixture.0.join("herdr");
    fs::write(&herdr, fs::read_to_string(&herdr).unwrap().replace("/repo", std::env::current_dir().unwrap().to_str().unwrap()).replace("case \"$1 $2\" in", "case \"$1 $2\" in\n  'workspace list') printf '%s\\n' '{\"result\":{\"workspaces\":[{\"workspace_id\":\"w1\"}]}}';;")).unwrap();
    let run = |args: &[&str]| {
        fixture
            .namedrop_command(args)
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_WORKSPACE_ID")
            .env_remove("HERDR_PANE_ID")
            .env_remove("HC_TERMINAL_ID")
            .output()
            .unwrap()
    };
    for (flag, value) in [
        ("--herdr-socket", "socket"),
        ("--herdr-workspace", "w1"),
        ("--herdr-pane", "p1"),
    ] {
        let activation = run(&[
            flag,
            value,
            "activate",
            "--input-json",
            "{\"confirmed\":true}",
        ]);
        assert!(activation.status.success(), "{flag}: {}", text(&activation));
        let output = run(&[flag, value, "address", "list"]);
        assert!(output.status.success(), "{flag}: {}", text(&output));
    }
    let hc = fixture.0.join("selection-hc");
    fs::write(
        &hc,
        format!(
            r#"#!/bin/sh
case "$1" in
get) cat '{}' ;;
ls) printf '%s\n' '{{"terminals":[]}}' ;;
server) printf '%s\n' '{{"core":{{"id":"selection"}}}}' ;;
*) exit 1 ;;
esac
"#,
            fixture.0.join("selection.json").display()
        ),
    )
    .unwrap();
    fs::set_permissions(&hc, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        fixture.0.join("selection.json"),
        include_str!("fixtures/hc/get-labeled.stdout"),
    )
    .unwrap();
    namedrop::StateStore::open(fixture.0.join("state"))
        .unwrap()
        .transaction(|tx| {
            tx.activate_worktree(
                &namedrop::StateNamespace::for_space("hc:selection", "fixture-space"),
                &namedrop::MutationAttribution::new(namedrop::ActorKind::Human, "test", 1),
            )
        })
        .unwrap();
    for (flag, value) in [("--hc-terminal", "t1"), ("--hc-space", "fixture-space")] {
        let output = fixture
            .namedrop_command(&[flag, value, "address", "list"])
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_PANE_ID")
            .env_remove("HC_TERMINAL_ID")
            .env("HC_BIN_PATH", &hc)
            .output()
            .unwrap();
        assert!(output.status.success(), "{flag}: {}", text(&output));
    }
    let default_host = run(&["address", "list"]);
    assert!(default_host.status.success(), "{}", text(&default_host));
    let override_host = run(&[
        "--host",
        "tui",
        "--hc-space",
        "ignored",
        "--herdr-workspace",
        "w1",
        "address",
        "list",
    ]);
    assert!(override_host.status.success(), "{}", text(&override_host));
}

#[test]
fn tui_ignores_hc_state_for_execution_and_command_guard() {
    let fixture = Fixture::new("tui-hc-state");
    fixture.activate();
    let state = fixture.0.join("state");
    let hc_state = fixture.0.join("hc-state");
    fs::create_dir_all(hc_state.join("runtime")).unwrap();
    fs::write(hc_state.join("runtime/namedrop"), "#!/bin/sh\nexit 91\n").unwrap();
    fs::set_permissions(
        hc_state.join("runtime/namedrop"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_namedrop"));
    fixture
        .host(&mut command)
        .env_remove("HERDR_PANE_ID")
        .env("HERDR_PLUGIN_STATE_DIR", &state)
        .env("NAMEDROP_STATE_DIR", &hc_state)
        .args(["address", "list"]);
    let output = command.output().unwrap();
    assert!(output.status.success(), "{}", text(&output));
    assert!(state.join("namedrop.sqlite3").exists() || fs::read_dir(&state).unwrap().count() > 0);
    assert_eq!(
        fs::read_dir(&hc_state).unwrap().count(),
        1,
        "HC state must not be opened"
    );
}

fn integration_snapshot(root: &Path) -> std::collections::BTreeMap<PathBuf, (Vec<u8>, u32)> {
    fn walk(
        root: &Path,
        path: &Path,
        entries: &mut std::collections::BTreeMap<PathBuf, (Vec<u8>, u32)>,
    ) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(root, &path, entries);
            } else {
                entries.insert(
                    path.strip_prefix(root).unwrap().to_owned(),
                    (
                        fs::read(&path).unwrap(),
                        fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    ),
                );
            }
        }
    }
    let mut entries = std::collections::BTreeMap::new();
    walk(root, root, &mut entries);
    entries
}

#[test]
fn hc_hook_popups_use_only_the_test_app() {
    app_popup::guard(
        &["hc_hooks_write_sessions_only_in_active_spaces_and_preserve_core_routing"],
        "hooks",
    );
}
